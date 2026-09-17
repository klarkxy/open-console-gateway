use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Duration, SecondsFormat, Utc};
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::{Database, ForwardLogQueryOptions};
use ocg_core::gateway;
use ocg_core::models::{
    Account, AccountSetupStep, AccountType, AccountUpdate, AppConfig, ProxyMode, RoutingMode,
};
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
    COMMAND_CODE_PROVIDER_ID, CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CredentialKind,
    OPENCODE_PROVIDER_ID, QuotaScope, ZEN_FREE_ACCOUNT_ID,
};
use ocg_core::state::CoreStateInner;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::net::TcpListener as StdTcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

#[path = "fixtures/gateway_fallback.rs"]
mod fallback_fix;

use fallback_fix::*;

#[tokio::test]
async fn fake_upstream_captures_protocol_auth_and_scripts_status_streams() {
    let replies = script(&[
        (
            "gemini-key",
            &[reply(
                StatusCode::OK.as_u16(),
                "data: {\"usageMetadata\":{\"promptTokenCount\":1}}\n\n",
            )],
        ),
        ("", &[reply(StatusCode::OK.as_u16(), r#"{"ok":true}"#)]),
    ]);
    let (base_url, calls, stop_fake) = start_fake_upstream(replies).await;
    let client = loopback_client();

    let gemini = client
        .post(format!(
            "{base_url}/v1beta/models/fake:streamGenerateContent"
        ))
        .header("x-goog-api-key", "gemini-key")
        .send()
        .await
        .unwrap();
    assert_eq!(gemini.status(), StatusCode::OK);
    assert!(gemini.text().await.unwrap().contains("usageMetadata"));
    assert_eq!(
        client
            .post(format!("{base_url}/zen/free"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].path, "/v1beta/models/fake:streamGenerateContent");
    assert_eq!(calls[0].x_goog_api_key.as_deref(), Some("gemini-key"));
    assert_eq!(calls[1].method, axum::http::Method::POST);
    assert!(calls[1].authorization.is_none());
    assert!(calls[1].x_api_key.is_none());
    assert!(calls[1].x_goog_api_key.is_none());
    drop(calls);
    let _ = stop_fake.send(());
}

#[tokio::test]
async fn model_discovery_returns_local_list_with_zero_accounts() {
    let h = FallbackHarness::go(
        &[(
            "key-1",
            &[reply(
                200,
                r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"}]}"#,
            )],
        )],
        &[],
    )
    .await;

    let unauthorized = loopback_client()
        .get(format!("http://127.0.0.1:{}/v1/models", h.port))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let invalid = loopback_client()
        .get(format!("http://127.0.0.1:{}/v1/models", h.port))
        .bearer_auth("wrong-key")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_local_openai_alias_list(&h.state, &body);
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "GET /v1/models must not call upstream: {:?}",
        h.calls.lock().unwrap()
    );
    assert!(h.state.db.lock().list_forward_logs(10).unwrap().is_empty());
}

#[tokio::test]
async fn model_discovery_publishes_saved_sealed_cn_aliases_without_raw_ids() {
    let p = PreparedFallback::go(&[], &[]).await;
    let now = chrono::Utc::now();
    {
        let db = p.state.db.lock();
        db.set_contract_catalog(
            &ocg_core::provider_contracts::ContractScope::provider(
                ocg_core::provider::MINIMAX_PROVIDER_ID,
            ),
            &[
                "MiniMax-M2.1".to_string(),
                "MiniMax-M2.1-highspeed".to_string(),
                "MiniMax-M2".to_string(),
            ],
            Some(now),
            ocg_core::provider_contracts::CATALOG_SOURCE_MINIMAX_CN_MODELS,
            "https://api.minimaxi.com/v1/models",
            now,
        )
        .unwrap();
        db.set_contract_catalog(
            &ocg_core::provider_contracts::ContractScope::provider(
                ocg_core::provider::KIMI_PROVIDER_ID,
            ),
            &[
                "kimi-for-coding-highspeed".to_string(),
                "k3".to_string(),
                "k3-256k".to_string(),
            ],
            Some(now),
            ocg_core::provider_contracts::CATALOG_SOURCE_KIMI_CN_MODELS,
            "https://api.kimi.com/coding/v1/models",
            now,
        )
        .unwrap();
    }
    p.state.reload_provider_contracts().unwrap();
    let h = p.bind().await;

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<HashSet<_>>();
    for alias in [
        "minimax-m2",
        "minimax-m2.1",
        "minimax-m2.1-highspeed",
        "kimi-k2.7-code-highspeed",
        "kimi-k3",
        "kimi-k3-256k",
    ] {
        assert!(ids.contains(alias), "missing {alias}: {body}");
    }
    for raw in [
        "MiniMax-M2",
        "MiniMax-M2.1",
        "kimi-for-coding-highspeed",
        "k3",
        "k3-256k",
    ] {
        assert!(!ids.contains(raw), "raw ID leaked into /v1/models: {body}");
    }
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "GET /v1/models must stay local: {:?}",
        h.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn model_discovery_publishes_enabled_goat_short_alias_without_raw_id() {
    const RAW: &str = "nvidia/nemotron-3-ultra-550b-a55b";
    const ALIAS: &str = "nemotron-3-ultra";

    let p = PreparedFallback::go(&[], &[]).await;
    p.state
        .activate_zen_free_model_catalog(ocg_core::kernel::zen::ZenFreeModelCatalog {
            models: Vec::new(),
            refreshed_at: Some(Utc::now()),
            source_url: ocg_core::kernel::zen::ZEN_MODELS_SOURCE_URL.to_string(),
        })
        .unwrap();
    persist_goat_verified_catalog(&p.state, "unused", &[RAW]);
    let h = p.bind().await;

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<HashSet<_>>();
    assert!(ids.contains(ALIAS), "missing GOAT short Alias: {body}");
    assert!(
        !ids.contains(RAW),
        "GOAT raw ID leaked into /v1/models: {body}"
    );

    disable_command_protocols(&h.state, RAW);
    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains(&format!("\"{ALIAS}\"")),
        "disabled GOAT Alias must leave /v1/models: {body}"
    );
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "GET /v1/models must stay local"
    );
}

#[tokio::test]
async fn model_discovery_does_not_create_inference_logs() {
    let p = PreparedFallback::go(
        &[(
            "key-1",
            &[reply(
                200,
                r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"vendor-raw-not-an-alias"}]}"#,
            )],
        )],
        &["key-1"],
    )
    .await;
    let before = p.state.db.lock().get_account("acct-1").unwrap().unwrap();
    let h = p.bind().await;

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK);
    assert_local_openai_alias_list(&h.state, &body);
    assert!(
        !body.contains("mimo-v2.5-free") && body.contains("mimo-v2.5"),
        "Zen Free must publish only the suffix-stripped Alias: {body}"
    );
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "GET /v1/models must not call upstream: {:?}",
        h.calls.lock().unwrap()
    );
    let logs = h
        .state
        .db
        .lock()
        .query_forward_logs(empty_forward_query())
        .unwrap();
    assert!(logs.items.is_empty());
    assert_eq!(logs.summary.total_requests, 0);
    let after = h.account("acct-1");
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.auth_error, before.auth_error);
    assert_eq!(after.updated_at, before.updated_at);
}

#[tokio::test]
async fn application_models_is_local_with_zero_accounts() {
    let h = FallbackHarness::go(
        &[(
            "key-1",
            &[reply(
                200,
                r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"vendor-raw-not-an-alias"}]}"#,
            )],
        )],
        &[],
    )
    .await;
    let routing_before = format!("{:?}", h.state.routing);

    let (status, body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids = application_model_ids(&body);
    assert_eq!(ids, expected_local_application_models(&h.state));
    assert!(ids.contains(&"deepseek-v4-flash".to_string()));
    assert!(!ids.contains(&"minimax-m2.7-highspeed".to_string()));
    assert!(!ids.iter().any(|id| id.contains('/')));
    assert!(
        !ids.iter()
            .any(|id| *id == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
    );
    assert!(!ids.iter().any(|id| id.ends_with("-free")));
    assert_no_application_model_side_effects(&h.state, &h.calls, None, &routing_before);
}

#[tokio::test]
async fn application_models_does_not_select_accounts_or_hit_upstream() {
    let p = PreparedFallback::go(&[("key-1", &[limited()])], &["key-1"]).await;
    let before = p.state.db.lock().get_account("acct-1").unwrap().unwrap();
    let routing_before = format!("{:?}", p.state.routing);
    let h = p.bind().await;

    let (status, body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["models"],
        serde_json::to_value(expected_local_application_models(&h.state)).unwrap()
    );
    assert_no_application_model_side_effects(&h.state, &h.calls, Some(&before), &routing_before);
}

#[tokio::test]
async fn application_models_intersects_priced_go_aliases_in_registry_order() {
    let p = PreparedFallback::go(
        &[(
            "key-1",
            &[reply(
                200,
                r#"{"object":"list","data":[{"id":"unknown"},{"id":"grok-4.5"},{"id":"kimi-k3"},{"id":"glm-5.1"},{"id":"minimax-m2.7-highspeed"},{"id":"minimax-m2.7"},{"id":"deepseek-v4-flash"},{"id":"minimax-m2.7"},{"id":"qwen3.7-plus"}]}"#,
            )],
        )],
        &["key-1"],
    )
    .await;
    let mut pricing = p.state.pricing_snapshot().as_ref().clone();
    pricing.models.retain(|model| {
        matches!(
            model.model_id.as_str(),
            "grok-4.5" | "kimi-k3" | "minimax-m2.7" | "glm-5.1"
        )
    });
    pricing.revision = format!("test-priced-models-{}", Utc::now().timestamp_micros());
    pricing.activated_at = Utc::now().to_rfc3339();
    p.state.activate_pricing_snapshot(pricing).unwrap();
    let before = p.state.db.lock().get_account("acct-1").unwrap().unwrap();
    let routing_before = format!("{:?}", p.state.routing);
    let h = p.bind().await;

    let (status, body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["models"],
        serde_json::json!(["glm-5.1", "grok-4.5", "kimi-k3", "minimax-m2.7"])
    );
    assert_eq!(
        application_model_ids(&body),
        expected_local_application_models(&h.state)
    );
    assert_no_application_model_side_effects(&h.state, &h.calls, Some(&before), &routing_before);
}

#[tokio::test]
async fn application_models_empty_intersection_returns_empty_list() {
    let p = PreparedFallback::go(
        &[(
            "key-1",
            &[reply(500, r#"{"error":"upstream unavailable"}"#)],
        )],
        &["key-1"],
    )
    .await;
    let mut empty = p.state.pricing_snapshot().as_ref().clone();
    let mut raw_row = empty.models[0].clone();
    raw_row.model_id = "vendor-raw-not-an-alias".into();
    empty.models.clear();
    empty.revision = format!("test-empty-pricing-{}", Utc::now().timestamp_micros());
    empty.activated_at = Utc::now().to_rfc3339();
    p.state.activate_pricing_snapshot(empty).unwrap();
    let before = p.state.db.lock().get_account("acct-1").unwrap().unwrap();
    let routing_before = format!("{:?}", p.state.routing);
    let h = p.bind().await;

    let (status, body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"], serde_json::json!([]));
    assert_no_application_model_side_effects(&h.state, &h.calls, Some(&before), &routing_before);

    let mut disjoint = h.state.pricing_snapshot().as_ref().clone();
    disjoint.models = vec![raw_row];
    disjoint.revision = format!("test-disjoint-pricing-{}", Utc::now().timestamp_micros());
    disjoint.activated_at = Utc::now().to_rfc3339();
    h.state.activate_pricing_snapshot(disjoint).unwrap();

    let (status, body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"], serde_json::json!([]));
    assert!(h.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn routes_all_client_formats_to_each_models_native_protocol() {
    for (client_path, model, upstream_path, upstream_body) in [
        (
            "/v1/chat/completions",
            "deepseek-v4-flash",
            "/v1/chat/completions",
            SUCCESS_BODY,
        ),
        (
            "/v1/chat/completions",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_SUCCESS_BODY,
        ),
        (
            "/v1/responses",
            "deepseek-v4-flash",
            "/v1/responses",
            RESPONSES_SUCCESS_BODY,
        ),
        ("/v1/responses", "hy3", "/v1/chat/completions", SUCCESS_BODY),
        (
            "/v1/responses",
            "glm-5.2",
            "/v1/chat/completions",
            SUCCESS_BODY,
        ),
        (
            "/v1/responses",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_SUCCESS_BODY,
        ),
        (
            "/v1/messages",
            "deepseek-v4-flash",
            "/v1/messages",
            MESSAGES_SUCCESS_BODY,
        ),
        ("/v1/messages", "hy3", "/v1/chat/completions", SUCCESS_BODY),
        (
            "/v1/messages",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_SUCCESS_BODY,
        ),
        (
            "/v1/messages",
            "glm-5.2",
            "/v1/chat/completions",
            SUCCESS_BODY,
        ),
    ] {
        let h = FallbackHarness::go(&[("key-1", &[reply(200, upstream_body)])], &["key-1"]).await;
        let (status, response) = h.protocol(client_path, model).await;
        assert_eq!(status, StatusCode::OK, "{client_path} {model}");

        let call = h.calls.lock().unwrap()[0].clone();
        assert_eq!(call.path, upstream_path);
        if upstream_path == "/v1/messages" {
            assert_eq!(call.x_api_key.as_deref(), Some("key-1"));
            assert!(call.authorization.is_none());
            assert_eq!(call.anthropic_version.as_deref(), Some("2023-06-01"));
        } else {
            assert_eq!(call.authorization.as_deref(), Some("Bearer key-1"));
            assert!(call.x_api_key.is_none());
            assert!(call.anthropic_version.is_none());
        }
        let upstream_request: serde_json::Value = serde_json::from_str(&call.body).unwrap();
        assert_eq!(upstream_request["model"], model);
        match upstream_path {
            "/v1/responses" => {
                assert!(
                    upstream_request.get("input").is_some(),
                    "Responses upstream should keep input: {}",
                    call.body
                );
                assert!(upstream_request.get("messages").is_none());
            }
            _ => assert!(upstream_request["messages"].is_array()),
        }

        match client_path {
            "/v1/chat/completions" => {
                assert_eq!(response["object"], "chat.completion");
                assert_eq!(response["choices"][0]["message"]["content"], "ok");
            }
            "/v1/responses" => {
                assert_eq!(response["object"], "response");
                assert_eq!(response["output"][0]["content"][0]["text"], "ok");
            }
            "/v1/messages" => {
                assert_eq!(response["type"], "message");
                assert_eq!(response["content"][0]["text"], "ok");
            }
            _ => unreachable!(),
        }
        let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_eq!((log.prompt_tokens, log.completion_tokens), (10, 2));
        assert_eq!(log.status, "success");
        assert_eq!(log.cost_state, "priced");
        assert!(log.cost.is_some());
        assert!(log.pricing_revision_id.is_some());
        assert!(
            log.request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("ocg-"))
        );
        assert_eq!(log.attempt, Some(1));
        assert!(log.error_source.is_none());
        assert!(log.error_stage.is_none());
        assert!(log.diagnostic.is_none());
    }
}

#[tokio::test]
async fn successful_inference_never_echoes_the_selected_account_key() {
    for client_path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let h = FallbackHarness::go(
            &[(
                OPAQUE_ACCOUNT_KEY,
                &[reply(200, SUCCESS_BODY_WITH_ECHOED_KEY)],
            )],
            &[OPAQUE_ACCOUNT_KEY],
        )
        .await;
        let (status, response) = h.protocol(client_path, "hy3").await;
        assert_eq!(status, StatusCode::OK, "{client_path}: {response}");
        assert!(
            !response.to_string().contains(OPAQUE_ACCOUNT_KEY),
            "{client_path} leaked the selected account Key: {response}"
        );
    }
}

#[tokio::test]
async fn common_short_key_redaction_preserves_non_stream_protocol_discriminators() {
    for client_path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let h = FallbackHarness::go(
            &[("text", &[reply(200, SUCCESS_BODY_WITH_COMMON_KEY)])],
            &["text"],
        )
        .await;
        let (status, response) = h.protocol(client_path, "hy3").await;
        assert_eq!(status, StatusCode::OK, "{client_path}: {response}");
        let content = match client_path {
            "/v1/chat/completions" => {
                assert_eq!(response["object"], "chat.completion");
                response["choices"][0]["message"]["content"].as_str()
            }
            "/v1/responses" => {
                assert_eq!(response["object"], "response");
                assert_eq!(response["output"][0]["type"], "message");
                assert_eq!(response["output"][0]["content"][0]["type"], "output_text");
                response["output"][0]["content"][0]["text"].as_str()
            }
            "/v1/messages" => {
                assert_eq!(response["type"], "message");
                assert_eq!(response["content"][0]["type"], "text");
                response["content"][0]["text"].as_str()
            }
            _ => unreachable!(),
        };
        assert_eq!(content, Some("before <redacted> after"), "{response}");
    }
}

#[tokio::test]
async fn non_stream_tool_argument_redaction_preserves_nested_json_keys() {
    let h = FallbackHarness::go(
        &[("data", &[reply(200, SUCCESS_BODY_WITH_NESTED_ARGUMENT_KEY)])],
        &["data"],
    )
    .await;
    let (status, response) = h
        .protocol("/v1/chat/completions", "deepseek-v4-flash")
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let arguments = response["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
        .as_str()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(arguments).unwrap(),
        serde_json::json!({"data":"safe","token":"<redacted>"})
    );
}

#[tokio::test]
async fn successful_conversion_redacts_a_key_before_opaque_reasoning_replay_encoding() {
    let h = FallbackHarness::go(
        &[(
            OPAQUE_ACCOUNT_KEY,
            &[reply(
                200,
                MESSAGES_SUCCESS_BODY_WITH_ECHOED_KEY_IN_THINKING,
            )],
        )],
        &[OPAQUE_ACCOUNT_KEY],
    )
    .await;
    let (status, response) = h.protocol("/v1/responses", "minimax-m2.7").await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let encrypted = response["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "reasoning")
        .and_then(|item| item["encrypted_content"].as_str())
        .expect("converted response should retain a safe reasoning replay block");
    let encoded = encrypted
        .strip_prefix("ocg-anthropic-thinking-v1:")
        .expect("reasoning replay should use the Anthropic envelope");
    let decoded = String::from_utf8(URL_SAFE_NO_PAD.decode(encoded).unwrap()).unwrap();
    assert!(
        !decoded.contains(OPAQUE_ACCOUNT_KEY),
        "opaque replay leaked the selected account Key: {decoded}"
    );
    assert!(decoded.contains("<redacted>"), "{decoded}");
}

#[tokio::test]
async fn streamed_inference_redacts_a_selected_key_split_across_events() {
    for client_path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let h = FallbackHarness::go(
            &[(
                OPAQUE_ACCOUNT_KEY,
                &[reply(200, CHAT_STREAM_WITH_SPLIT_ECHOED_KEY)],
            )],
            &[OPAQUE_ACCOUNT_KEY],
        )
        .await;
        let (status, body) = h.stream(client_path, "hy3").await;
        assert_eq!(status, StatusCode::OK, "{client_path}: {body}");
        assert!(
            !body.contains(OPAQUE_ACCOUNT_KEY),
            "{client_path} leaked a split selected account Key: {body}"
        );
        assert!(body.contains("before "), "{client_path}: {body}");
        assert!(body.contains(" after"), "{client_path}: {body}");
    }
}

#[tokio::test]
async fn common_short_key_redaction_preserves_stream_protocol_discriminators() {
    for client_path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let h = FallbackHarness::go(
            &[("text", &[reply(200, CHAT_STREAM_WITH_COMMON_KEY)])],
            &["text"],
        )
        .await;
        let (status, body) = h.stream(client_path, "hy3").await;
        assert_eq!(status, StatusCode::OK, "{client_path}: {body}");
        assert!(!body.contains("before text after"), "{client_path}: {body}");
        assert!(body.contains("before "), "{client_path}: {body}");
        assert!(body.contains(" after"), "{client_path}: {body}");
        match client_path {
            "/v1/chat/completions" => {
                assert!(body.contains("chat.completion.chunk"), "{body}")
            }
            "/v1/responses" => {
                assert!(body.contains("response.output_text.delta"), "{body}")
            }
            "/v1/messages" => assert!(body.contains("text_delta"), "{body}"),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn inference_skips_accounts_with_unusable_stored_credentials() {
    for (client_path, model, upstream_path, upstream_body) in [
        (
            "/v1/chat/completions",
            "deepseek-v4-flash",
            "/v1/chat/completions",
            SUCCESS_BODY,
        ),
        (
            "/v1/responses",
            "deepseek-v4-flash",
            "/v1/responses",
            RESPONSES_SUCCESS_BODY,
        ),
        (
            "/v1/messages",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_SUCCESS_BODY,
        ),
    ] {
        let p = PreparedFallback::go(
            &[("key-good", &[reply(200, upstream_body)])],
            &["placeholder", "bad\nheader", "key-good"],
        )
        .await;
        corrupt_account_cipher(&p.state, "acct-1", "!!!not-base64!!!");
        let h = p.bind().await;

        let (status, _) = h.protocol(client_path, model).await;
        assert_eq!(status, StatusCode::OK, "{client_path}");
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{client_path}");
        assert_eq!(calls[0].key, "key-good", "{client_path}");
        assert_eq!(calls[0].path, upstream_path, "{client_path}");
        drop(calls);
        let logs = h.state.db.lock().list_forward_logs(10).unwrap();
        assert_eq!(logs.len(), 3, "{client_path}");
        let success = logs
            .iter()
            .find(|log| log.status == "success")
            .expect("successful fallback attempt should be logged");
        assert_eq!(success.account_id, "acct-3", "{client_path}");
        let request_id = success.request_id.as_deref().unwrap();
        assert!(
            logs.iter()
                .all(|log| log.request_id.as_deref() == Some(request_id))
        );
        let mut attempts = logs
            .iter()
            .filter_map(|log| log.attempt)
            .collect::<Vec<_>>();
        attempts.sort_unstable();
        assert_eq!(attempts, [1, 2, 3]);
        let credential_failures = logs
            .iter()
            .filter(|log| log.error_stage.as_deref() == Some("credential"))
            .collect::<Vec<_>>();
        assert_eq!(credential_failures.len(), 2);
        assert!(
            credential_failures
                .iter()
                .all(|log| log.diagnostic.is_some())
        );
    }
}

#[tokio::test]
async fn converts_streams_across_chat_messages_and_responses() {
    for (client_path, model, upstream_path, upstream_body, expected_events) in [
        (
            "/v1/messages",
            "hy3",
            "/v1/chat/completions",
            CHAT_STREAM_BODY,
            &["event: message_start", "text_delta", "event: message_stop"][..],
        ),
        (
            "/v1/responses",
            "hy3",
            "/v1/chat/completions",
            CHAT_STREAM_BODY,
            &[
                "event: response.created",
                "response.output_text.delta",
                "event: response.completed",
            ][..],
        ),
        (
            "/v1/chat/completions",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_STREAM_BODY,
            &["finish_reason", "data: [DONE]"][..],
        ),
        (
            "/v1/responses",
            "minimax-m2.7",
            "/v1/messages",
            MESSAGES_STREAM_BODY,
            &[
                "event: response.created",
                "response.output_text.delta",
                "event: response.completed",
            ][..],
        ),
    ] {
        let h = FallbackHarness::go(&[("key-1", &[reply(200, upstream_body)])], &["key-1"]).await;
        let (status, body) = h.stream(client_path, model).await;
        assert_eq!(status, StatusCode::OK);
        for expected in expected_events {
            assert!(
                body.contains(expected),
                "{client_path} {model} missing {expected}: {body}"
            );
        }
        if client_path == "/v1/chat/completions" {
            assert_eq!(chat_stream_text(&body), "ok", "{body}");
        }
        assert_eq!(h.calls.lock().unwrap()[0].path, upstream_path);
        let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_eq!((log.prompt_tokens, log.completion_tokens), (10, 2));
        assert_eq!(log.status, "success");
    }
}

#[tokio::test]
async fn stream_can_outlive_non_stream_timeout() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "text/event-stream",
        vec![vec![
            (StdDuration::ZERO, MESSAGES_STREAM_HEAD),
            (StdDuration::from_millis(1_200), MESSAGES_STREAM_TAIL),
        ]],
        &["key-1"],
        |config| {
            config.non_stream_timeout_secs = 1;
            config.stream_idle_timeout_secs = 2;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(4),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("stream should finish before the test watchdog");
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: message_stop"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "success");
    assert_eq!((log.prompt_tokens, log.completion_tokens), (10, 2));
}

#[tokio::test]
async fn non_stream_uses_non_stream_timeout_not_stream_idle_timeout() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "application/json",
        vec![vec![(
            StdDuration::from_millis(1_200),
            MESSAGES_SUCCESS_BODY,
        )]],
        &["key-1"],
        |config| {
            config.non_stream_timeout_secs = 3;
            config.stream_idle_timeout_secs = 1;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("non-stream response should finish before the test watchdog");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["content"][0]["text"], serde_json::json!("ok"));
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "success");
}

#[tokio::test]
async fn streamed_request_with_non_sse_success_body_timeout_is_not_replayed() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "application/json",
        vec![vec![(StdDuration::from_secs(10), MESSAGES_SUCCESS_BODY)]],
        &["key-1", "key-2"],
        |config| {
            config.stream_idle_timeout_secs = 1;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("non-SSE stream response should honor the idle timeout");
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert!(body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
    assert_eq!(log.http_status, Some(200));
}

#[tokio::test]
async fn streamed_request_with_stalled_error_body_returns_status_without_replay() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::INTERNAL_SERVER_ERROR,
        "application/json",
        vec![vec![(
            StdDuration::from_secs(10),
            r#"{"error":"late failure details"}"#,
        )]],
        &["key-1", "key-2"],
        |config| {
            config.stream_idle_timeout_secs = 1;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("error response body should honor the idle timeout");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert!(body.to_ascii_lowercase().contains("timed out"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "error");
}

#[tokio::test]
async fn stream_idle_timeout_emits_protocol_error_and_updates_log() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "text/event-stream",
        vec![vec![
            (StdDuration::ZERO, MESSAGES_STREAM_HEAD),
            (StdDuration::from_secs(10), MESSAGES_STREAM_TAIL),
        ]],
        &["key-1"],
        |config| {
            config.stream_idle_timeout_secs = 1;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(8),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("idle timeout should finish before the test watchdog");
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: error"), "{body}");
    assert!(body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
    assert_eq!(log.cost_state, "outcome_unknown");
    assert_eq!(log.cost, None);
    assert!(log.error_message.is_some());
}

#[tokio::test]
async fn non_stream_body_timeout_is_outcome_unknown_and_is_not_replayed() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "application/json",
        vec![vec![(
            StdDuration::from_millis(1_200),
            MESSAGES_SUCCESS_BODY,
        )]],
        &["key-1"],
        |config| {
            config.non_stream_timeout_secs = 1;
        },
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("non-stream timeout should finish before the test watchdog");
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(
        body["error"]["type"],
        serde_json::json!("upstream_outcome_unknown")
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    let message = message.to_ascii_lowercase();
    assert!(
        message.contains("timeout") || message.contains("timed out"),
        "{body}"
    );
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
    assert_eq!(log.cost_state, "outcome_unknown");
    assert_eq!(log.cost, None);
}

#[tokio::test]
async fn truncated_non_stream_success_body_is_outcome_unknown_and_not_replayed() {
    let raw_response = concat!(
        "HTTP/1.1 200 OK\r\n",
        "content-type: application/json\r\n",
        "content-length: 4096\r\n",
        "connection: close\r\n",
        "\r\n",
        "{\"id\":\"partial"
    )
    .as_bytes()
    .to_vec();
    let h = FallbackHarness::disconnect(raw_response, &["key-1", "key-2"]).await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("truncated body should fail before the watchdog");
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(
        body["error"]["type"],
        serde_json::json!("upstream_outcome_unknown")
    );
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
}

#[tokio::test]
async fn interrupted_stream_is_outcome_unknown_and_not_replayed() {
    let payload = MESSAGES_STREAM_HEAD;
    let raw_response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n{:X}\r\n{}\r\n",
        payload.len(),
        payload
    )
    .into_bytes();
    let h = FallbackHarness::disconnect(raw_response, &["key-1", "key-2"]).await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("interrupted stream should fail before the watchdog");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
}

#[tokio::test]
async fn stream_ending_before_downstream_output_retries_same_account_once() {
    let h = FallbackHarness::delayed_seq(
        StatusCode::OK,
        "text/event-stream",
        vec![Vec::new(), vec![(StdDuration::ZERO, CHAT_STREAM_BODY)]],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/chat/completions", "deepseek-v4-flash"),
    )
    .await
    .expect("the zero-output retry should complete before the watchdog");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(chat_stream_text(&body), "ok", "{body}");
    assert!(!body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 2);

    let mut logs = h.state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.attempt);
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|log| log.account_id == "acct-1"));
    assert_eq!(logs[0].status, "outcome_unknown");
    assert!(
        logs[1].status.starts_with("success"),
        "unexpected successful retry status: {}",
        logs[1].status
    );
    assert_eq!(logs[0].request_id, logs[1].request_id);
    assert_eq!(
        logs[0]
            .diagnostic
            .as_ref()
            .and_then(|value| value.get("retry_action"))
            .and_then(serde_json::Value::as_str),
        Some("retry_same_account")
    );
}

#[tokio::test]
async fn r06_zero_output_sse_retry_does_not_resend_after_rotation() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state_slot: Arc<Mutex<Option<Arc<CoreStateInner>>>> = Arc::new(Mutex::new(None));
    #[derive(Clone)]
    struct MutatingEmptySse {
        hits: Arc<std::sync::atomic::AtomicUsize>,
        state: Arc<Mutex<Option<Arc<CoreStateInner>>>>,
    }
    async fn mutating_empty_sse(
        axum::extract::State(state): axum::extract::State<MutatingEmptySse>,
    ) -> impl IntoResponse {
        let n = state.hits.fetch_add(1, Ordering::SeqCst);
        if n == 0
            && let Some(host) = state.state.lock().unwrap().clone()
        {
            let rotated = host.encrypt_key("sk-test-rotated-retry").unwrap();
            host.db
                .lock()
                .rotate_account_credential("acct-1", &rotated)
                .unwrap();
        }
        (StatusCode::OK, [("content-type", "text/event-stream")], "")
    }
    let app = Router::new()
        .fallback(axum::routing::any(mutating_empty_sse))
        .with_state(MutatingEmptySse {
            hits: hits.clone(),
            state: state_slot.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await;
    });
    let (state, dir) = build_state(format!("http://{addr}"), &["key-1"]);
    *state_slot.lock().unwrap() = Some(state.clone());
    let h = FallbackHarness::from_state(state, dir).await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/chat/completions", "deepseek-v4-flash"),
    )
    .await
    .expect("the zero-output retry should complete before the watchdog");
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "rotated captured retry must not send a second upstream request: {body}"
    );
    assert!(!body.contains("sk-test-rotated-retry"), "{body}");
    assert!(!body.contains("key-1"), "{body}");
    let logs = h.logs();
    assert!(
        logs.iter().any(|log| {
            log.diagnostic
                .as_ref()
                .and_then(|value| value.get("retry_action"))
                .and_then(serde_json::Value::as_str)
                == Some("retry_same_account")
        }),
        "expected a real RetrySameAccount attempt: {logs:?}"
    );

    let _ = stop_tx.send(());
}

#[tokio::test]
async fn stream_ending_twice_before_downstream_output_stops_after_one_retry() {
    let h = FallbackHarness::delayed_seq(
        StatusCode::OK,
        "text/event-stream",
        vec![Vec::new(), Vec::new()],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = tokio::time::timeout(
        StdDuration::from_secs(5),
        protocol_stream_call(h.port, "/v1/chat/completions", "deepseek-v4-flash"),
    )
    .await
    .expect("the bounded retry should finish before the watchdog");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 2);

    let mut logs = h.state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.attempt);
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|log| log.account_id == "acct-1"));
    let retry_actions = logs
        .iter()
        .map(|log| {
            log.diagnostic
                .as_ref()
                .and_then(|value| value.get("retry_action"))
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(retry_actions, [Some("retry_same_account"), Some("return")]);
}

#[tokio::test]
async fn upstream_408_is_outcome_unknown_and_does_not_fail_over() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(408, r#"{"error":{"message":"request timed out"}}"#)],
            ),
            ("key-2", &[ok_messages()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = h.protocol("/v1/messages", "minimax-m2.7").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{body}");
    assert_eq!(
        body["error"]["type"],
        serde_json::json!("upstream_outcome_unknown")
    );
    assert_eq!(h.call_count(), 1);
    assert_eq!(h.logs()[0].status, "outcome_unknown");
}

#[tokio::test]
async fn connect_failure_retries_once_without_account_fallback() {
    let (state, dir) = build_state(
        format!("http://127.0.0.1:{}", free_port()),
        &["key-1", "key-2"],
    );
    let mut config = state.config();
    config.connect_timeout_secs = 1;
    state.set_config(config).unwrap();
    let h = FallbackHarness::from_state(state, dir).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/messages", h.port))
        .header("x-api-key", "gw-test")
        .json(&serde_json::json!({
            "model": "minimax-m2.7",
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let response_request_id = response
        .headers()
        .get("x-ocg-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let logs = h.logs();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|log| log.account_id == "acct-1"));
    assert!(logs.iter().all(|log| log.status == "error"));
    assert!(
        logs.iter()
            .all(|log| log.request_id.as_deref() == Some(&response_request_id))
    );
    let mut attempts = logs
        .iter()
        .filter_map(|log| log.attempt)
        .collect::<Vec<_>>();
    attempts.sort_unstable();
    assert_eq!(attempts, [1, 2]);
    assert!(logs.iter().all(|log| {
        log.diagnostic
            .as_ref()
            .and_then(|value| value.get("request_id"))
            .and_then(serde_json::Value::as_str)
            == Some(response_request_id.as_str())
    }));
    let mut retry_actions = logs
        .iter()
        .filter_map(|log| {
            Some((
                log.attempt?,
                log.diagnostic
                    .as_ref()?
                    .get("retry_action")?
                    .as_str()?
                    .to_string(),
            ))
        })
        .collect::<Vec<_>>();
    retry_actions.sort_by_key(|(attempt, _)| *attempt);
    assert_eq!(
        retry_actions,
        [
            (1, "retry_same_account".to_string()),
            (2, "return".to_string())
        ]
    );
}

#[tokio::test]
async fn streaming_connect_failure_is_safe_to_retry_once() {
    let (state, dir) = build_state(
        format!("http://127.0.0.1:{}", free_port()),
        &["key-1", "key-2"],
    );
    let mut config = state.config();
    config.connect_timeout_secs = 1;
    state.set_config(config).unwrap();
    let h = FallbackHarness::from_state(state, dir).await;

    let (status, _) = h.stream("/v1/messages", "minimax-m2.7").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let logs = h.logs();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().all(|log| log.account_id == "acct-1"));
}

#[tokio::test]
async fn converted_messages_request_does_not_replay_upstream_5xx() {
    let h = FallbackHarness::go(
        &[
            ("key-1", &[reply(500, r#"{"error":"temporary"}"#)]),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = h.protocol("/v1/messages", "hy3").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["type"], "error");
    let calls = h.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.key.as_str())
            .collect::<Vec<_>>(),
        ["key-1"]
    );
    assert!(calls.iter().all(|call| call.path == "/v1/chat/completions"));
}

#[tokio::test]
async fn manual_order_drives_fallback_while_ineligible_accounts_are_skipped() {
    let p = PreparedFallback::go(
        &[
            (
                "key-2",
                &[reply(403, r#"{"error":{"message":"forbidden key"}}"#)],
            ),
            ("key-1", &[ok()]),
        ],
        &["key-1", "key-2", "key-3", "key-4"],
    )
    .await;
    {
        let db = p.state.db.lock();
        db.reorder_accounts(&[
            "acct-4".into(),
            "acct-3".into(),
            "acct-2".into(),
            "acct-1".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .unwrap();
        db.update_account(
            "acct-4",
            &AccountUpdate {
                name: None,
                username: None,
                password: None,
                key: None,
                enabled: Some(false),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            None,
        )
        .unwrap();
        db.set_account_cooldown(
            "acct-3",
            Some(Utc::now() + Duration::hours(1)),
            Some("test cooldown"),
        )
        .unwrap();
    }
    let h = p.bind().await;

    let (status, _) = h.chat().await;
    assert_eq!(status, 200);
    assert_eq!(h.call_keys(), ["key-2", "key-1"]);
}

#[tokio::test]
async fn converted_request_error_uses_callers_envelope_without_fallback() {
    let h = FallbackHarness::go(
        &[
            (
                OPAQUE_ACCOUNT_KEY,
                &[reply(400, ERROR_BODY_WITH_ECHOED_KEY)],
            ),
            ("key-2", &[ok()]),
        ],
        &[OPAQUE_ACCOUNT_KEY, "key-2"],
    )
    .await;

    let (status, body) = h.protocol("/v1/messages", "deepseek-v4-flash").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "error");
    assert!(!body.to_string().contains(OPAQUE_ACCOUNT_KEY));
    assert_eq!(h.call_count(), 1);

    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    let persisted = format!("{:?}{:?}", log.error_message, log.diagnostic);
    assert!(
        !persisted.contains(OPAQUE_ACCOUNT_KEY),
        "forward log leaked key: {persisted}"
    );
    assert!(log.diagnostic.is_some());
}

#[tokio::test]
async fn unterminated_stream_tail_never_echoes_the_selected_account_key() {
    let h = FallbackHarness::go(
        &[(
            OPAQUE_ACCOUNT_KEY,
            &[reply(200, CHAT_STREAM_WITH_UNTERMINATED_KEY_TAIL)],
        )],
        &[OPAQUE_ACCOUNT_KEY],
    )
    .await;
    let (status, body) = h.stream("/v1/chat/completions", "deepseek-v4-flash").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains(OPAQUE_ACCOUNT_KEY),
        "stream leaked key: {body}"
    );
}

#[tokio::test]
async fn request_larger_than_16_mib_reaches_upstream() {
    let h = FallbackHarness::go(&[("key-1", &[ok()])], &["key-1"]).await;
    let body = serde_json::json!({
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": "x".repeat(17 * 1024 * 1024)}],
        "max_tokens": 3,
        "stream": false
    });
    let response = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .bearer_auth("gw-test")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(h.call_count(), 1);
}

#[tokio::test]
async fn upstream_payload_too_large_is_not_mislabeled_as_client_body_limit() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(
                    413,
                    r#"{"error":{"message":"provider input too large"}}"#,
                )],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/messages", h.port))
        .header("x-api-key", "gw-test")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let request_id = response
        .headers()
        .get("x-ocg-request-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(h.call_count(), 1);

    let forward_logs = h.state.db.lock().list_forward_logs(10).unwrap();
    assert_eq!(forward_logs.len(), 1);
    assert_eq!(
        forward_logs[0].request_id.as_deref(),
        Some(request_id.as_str())
    );
    assert_eq!(forward_logs[0].error_source.as_deref(), Some("upstream"));
    assert_eq!(
        forward_logs[0].error_stage.as_deref(),
        Some("upstream_http")
    );
    assert!(
        h.state
            .db
            .lock()
            .query_gateway_logs(10, Some(&request_id))
            .unwrap()
            .is_empty(),
        "upstream 413 must not create a second client/body_limit diagnostic"
    );
}

#[tokio::test]
async fn falls_back_past_five_limited_accounts_to_sixth_success() {
    let keys = ["key-1", "key-2", "key-3", "key-4", "key-5", "key-6"];
    let queued = keys
        .iter()
        .enumerate()
        .map(|(idx, key)| {
            (
                *key,
                if idx == 5 {
                    vec![ok()]
                } else {
                    vec![limited()]
                },
            )
        })
        .collect::<Vec<_>>();
    let entries = queued
        .iter()
        .map(|(key, replies)| (*key, replies.as_slice()))
        .collect::<Vec<_>>();
    let h = FallbackHarness::go(&entries, &keys).await;

    let (status, _) = h.chat().await;
    assert_eq!(status, 200);
    assert_eq!(
        h.call_keys(),
        keys.iter().map(|k| k.to_string()).collect::<Vec<_>>()
    );
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| c.accept_encoding.as_deref() == Some("identity"))
    );

    let db = h.state.db.lock();
    let accounts = db.list_accounts().unwrap();
    assert_eq!(
        accounts
            .iter()
            .filter(|a| a.cooldown_until.is_some())
            .count(),
        5
    );
    let logs = db.list_forward_logs(20).unwrap();
    assert!(
        logs.iter()
            .any(|l| l.account_name == "acct-6" && l.status == "success")
    );
}

#[tokio::test]
async fn upstream_5xx_is_returned_without_same_account_retry_or_fallback() {
    let h = FallbackHarness::go(
        &[
            ("key-1", &[reply(500, r#"{"error":"temporary"}"#)]),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    let (status, _) = h.chat().await;
    assert_eq!(status, 500);
    assert_eq!(h.call_keys(), ["key-1"]);
}

#[tokio::test]
async fn inference_403_fails_over_without_persisting_an_auth_breaker() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(403, r#"{"error":{"message":"forbidden key"}}"#)],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    for _ in 0..2 {
        let (status, body) = h.chat().await;
        assert_eq!(status, 200, "{body}");
    }
    assert_eq!(h.call_keys(), ["key-1", "key-2", "key-1", "key-2"]);
    assert!(
        h.account("acct-1").auth_error.is_none(),
        "inference 403 must not permanently break an account"
    );
}

#[tokio::test]
async fn opencode_model_error_401_is_returned_without_failover_or_breaker() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(
                    401,
                    r#"{"type":"error","error":{"type":"ModelError","message":"Model is not supported"}}"#,
                )],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    for _ in 0..2 {
        let (status, body) = h.chat().await;
        assert_eq!(status, 401, "{body}");
        assert!(
            body.contains("ModelError") || body.contains("401"),
            "{body}"
        );
    }
    assert_eq!(h.call_keys(), ["key-1", "key-1"]);
    assert!(
        h.account("acct-1").auth_error.is_none(),
        "OpenCode ModelError 401 must not permanently break an account"
    );
}

#[tokio::test]
async fn opencode_credits_error_401_breaks_current_account_and_falls_through() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(
                    401,
                    r#"{"type":"error","error":{"type":"CreditsError","message":"No active subscription"}}"#,
                )],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    for _ in 0..2 {
        let (status, body) = h.chat().await;
        assert_eq!(status, 200, "{body}");
    }
    assert_eq!(h.call_keys(), ["key-1", "key-2", "key-2"]);
    let broken = h.account("acct-1");
    assert!(
        broken
            .auth_error
            .as_deref()
            .is_some_and(|error| error.contains("account error 401")),
        "CreditsError must persist an account-level breaker: {broken:?}"
    );
}

#[tokio::test]
async fn unknown_model_is_rejected_before_any_upstream_attempt() {
    let h = FallbackHarness::go(
        &[
            (
                "key-1",
                &[reply(
                    401,
                    r#"{"error":{"message":"model does not exist"}}"#,
                )],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;
    let runtime_log_watermark = h
        .state
        .db
        .lock()
        .list_gateway_logs(1)
        .unwrap()
        .first()
        .map_or(0, |log| log.id);

    let (status, body) = h
        .protocol("/v1/chat/completions", "totally-made-up-xyz")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("unknown model"), "{body}");
    assert!(h.calls.lock().unwrap().is_empty());
    let db = h.state.db.lock();
    let request_logs = db.list_forward_logs(10).unwrap();
    assert_eq!(
        request_logs.len(),
        1,
        "unknown model is still a client request"
    );
    assert_eq!(request_logs[0].status, "client_error");
    assert_eq!(request_logs[0].http_status, Some(400));
    assert_eq!(request_logs[0].error_source.as_deref(), Some("client"));
    assert_eq!(request_logs[0].error_stage.as_deref(), Some("validation"));
    let new_runtime_logs = db
        .list_gateway_logs(10)
        .unwrap()
        .into_iter()
        .filter(|log| log.id > runtime_log_watermark)
        .collect::<Vec<_>>();
    assert!(
        new_runtime_logs.iter().all(|log| {
            log.category == "usage_sync" && log.request_id.is_none() && log.error_stage.is_none()
        }),
        "request validation must not add gateway runtime logs: {new_runtime_logs:?}"
    );
    drop(db);
    assert!(
        h.account("acct-1").auth_error.is_none(),
        "a rejected unknown model must not touch account state"
    );
}

#[tokio::test]
async fn corrupt_selectable_credential_writes_a_preflight_row_without_upstream_call() {
    let p = PreparedFallback::go(&[("key-2", &[ok()])], &["key-1", "key-2"]).await;
    corrupt_account_cipher(&p.state, "acct-1", "not-a-valid-cipher");
    let corrupted = p.state.db.lock().get_account("acct-1").unwrap().unwrap();
    assert!(corrupted.enabled, "{corrupted:?}");
    assert_eq!(corrupted.key_cipher, "not-a-valid-cipher");
    let h = p.bind().await;

    let (status, body) = h.chat().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_count(), 1);
    assert_eq!(h.call_keys(), ["key-2"]);

    let mut logs = h.logs();
    logs.sort_by_key(|log| log.attempt);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert_eq!(logs[0].status, "error");
    assert_eq!(logs[0].http_status, None);
    assert_eq!(logs[0].error_source.as_deref(), Some("gateway"));
    assert_eq!(logs[0].error_stage.as_deref(), Some("credential"));
    assert!(
        logs[0]
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("failed to decrypt account credentials")),
        "{:?}",
        logs[0].error_message
    );
    assert!(
        logs[1].account_id == "acct-2" && logs[1].status.starts_with("success"),
        "{logs:?}"
    );
}

#[tokio::test]
async fn registered_zen_promo_routes_to_zen_not_go() {
    let h = FallbackHarness::zen_go(&[("", &[ok(), ok()])], &["key-1"]).await;
    for model in ["mimo-v2.5-free", "mimo-v2.5"] {
        let (status, body) = h.protocol("/v1/chat/completions", model).await;
        assert_eq!(status, StatusCode::OK, "{model} {body}");
    }
    assert_eq!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>(),
        ["/zen/v1/chat/completions", "/zen/v1/chat/completions"]
    );
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.key.is_empty())
    );
}

#[tokio::test]
async fn unregistered_free_suffix_without_protocol_is_rejected_locally() {
    let h = FallbackHarness::zen_go(&[("key-1", &[ok_responses()])], &["key-1"]).await;
    let (status, body) = h
        .protocol("/v1/chat/completions", "brand-new-promo-free")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(h.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unknown_zen_catalog_free_id_forwards_as_chat_on_raw_pin_and_stripped_alias() {
    let p = PreparedFallback::zen_go(&[("", &[ok(), ok()])], &["normal-key"]).await;
    let mut catalog = (*p.state.zen_free_model_catalog()).clone();
    catalog.models.push("brand-new-promo-free".into());
    catalog.refreshed_at = Some(Utc::now());
    p.state
        .db
        .lock()
        .set_zen_free_model_catalog(&catalog)
        .unwrap();
    p.state.reload_provider_contracts().unwrap();
    p.state.activate_zen_free_model_catalog(catalog).unwrap();
    let h = p.bind().await;

    for model in ["brand-new-promo-free", "brand-new-promo"] {
        let (status, body) = h.protocol("/v1/chat/completions", model).await;
        assert_eq!(status, StatusCode::OK, "{model} {body}");
    }
    assert_eq!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>(),
        ["/zen/v1/chat/completions", "/zen/v1/chat/completions"]
    );
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.key.is_empty())
    );

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<HashSet<_>>();
    assert!(
        ids.contains("brand-new-promo"),
        "stripped Alias missing: {body}"
    );
    assert!(
        !ids.contains("brand-new-promo-free"),
        "raw pin leaked into /v1/models: {body}"
    );
}

#[tokio::test]
async fn registered_zen_model_401_is_returned_without_credential_fallback_or_breaker() {
    let h = FallbackHarness::zen_go(
        &[
            ("", &[reply(401, r#"{"error":{"message":"expired key"}}"#)]),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(h.call_keys(), [""]);
    assert!(
        h.account("acct-1").auth_error.is_none(),
        "inference 401 must not permanently break an account"
    );
}

#[tokio::test]
async fn all_limited_accounts_return_429_with_soonest_reset() {
    let h = FallbackHarness::go(
        &[("key-1", &[limited()]), ("key-2", &[limited()])],
        &["key-1", "key-2"],
    )
    .await;

    let (status, body) = h.chat().await;
    assert_eq!(status, 429);
    assert!(body.contains("resets_at"));
    assert_eq!(
        h.state
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .iter()
            .filter(|a| a.cooldown_until.is_some())
            .count(),
        2
    );
}

#[tokio::test]
async fn zen_free_429_is_anonymous_and_cools_the_singleton_egress_route() {
    let h = FallbackHarness::zen_go(
        &[("", &[limited()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
    )
    .await;

    let (status, _) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        h.call_keys(),
        [""],
        "Zen Free must not borrow or rotate an account key"
    );
    {
        let db = h.state.db.lock();
        let source = db.get_account(ZEN_FREE_ACCOUNT_ID).unwrap().unwrap();
        assert!(source.cooldown_free_until.is_some());
        assert!(source.cooldown_5h_until.is_none());
        assert!(source.cooldown_week_until.is_none());
        assert!(source.cooldown_month_until.is_none());
        assert!(db.free_channel_cooldown_until().unwrap().is_some());
        assert!(
            db.get_account("acct-1")
                .unwrap()
                .unwrap()
                .cooldown_until
                .is_none()
        );
    }
    let captured = h.calls.lock().unwrap()[0].clone();
    assert!(captured.authorization.is_none());
    assert!(captured.x_api_key.is_none());
    assert!(captured.x_goog_api_key.is_none());

    h.set_enabled("acct-1", false);
    let (status, _) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.call_count(), 1);

    h.state.db.lock().delete_account("acct-1").unwrap();
    let (status, _) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(h.call_count(), 1);
}

#[tokio::test]
async fn zen_free_is_anonymous_across_all_client_formats_and_logs_route_identity() {
    let h = FallbackHarness::zen_go(&[("", &[ok(), ok(), ok(), ok()])], &["normal-key"]).await;

    for path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let (status, body) = h.protocol(path, "mimo-v2.5-free").await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    let (status, body) = gemini_call(h.port, "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(captured.len(), 4);
    assert!(captured.iter().all(|call| {
        call.authorization.is_none() && call.x_api_key.is_none() && call.x_goog_api_key.is_none()
    }));
    assert!(
        captured
            .iter()
            .all(|call| call.path.ends_with("/v1/chat/completions"))
    );
    let logs = h.logs();
    assert_eq!(logs.len(), 4);
    assert!(logs.iter().all(|log| {
        log.route_account_id.as_deref() == Some(ZEN_FREE_ACCOUNT_ID)
            && log.provider_id.as_deref() == Some("opencode-zen-free")
            && log.credential_account_id.is_none()
            && log.account_id == ZEN_FREE_ACCOUNT_ID
    }));
    assert!(logs.iter().all(|log| {
        log.status == "success"
            && log.cost_state == "free"
            && log.raw_cost_usd == Some(0.0)
            && log.quota_debit == Some(0.0)
            && log.effective_paid_cost_usd == Some(0.0)
            && log.pricing_revision_id.is_none()
            && log.quota_multiplier.is_none()
            && log.local_adjustment_multiplier.is_none()
    }));
}

#[tokio::test]
async fn zen_free_synthesizes_official_anonymous_identity_headers() {
    let h = FallbackHarness::zen_go(&[("", &[ok()])], &["normal-key"]).await;
    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let call = h.calls.lock().unwrap()[0].clone();
    assert!(call.authorization.is_none());
    assert!(call.x_api_key.is_none());
    assert_eq!(call.opencode_client.as_deref(), Some("cli"));
    assert_eq!(call.user_agent.as_deref(), Some("opencode"));
    let session = call
        .opencode_session
        .as_deref()
        .expect("Zen Free must send x-opencode-session");
    assert!(!session.is_empty());
    assert_eq!(call.opencode_project.as_deref(), Some(session));
    assert!(
        call.opencode_request
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    );
}

#[tokio::test]
async fn zen_free_preserves_official_identity_and_replaces_foreign_user_agent() {
    let h = FallbackHarness::zen_go(&[("", &[ok(), ok()])], &["normal-key"]).await;

    let official = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .header(reqwest::header::USER_AGENT, "opencode/1.17.7")
        .header("x-opencode-session", "ses_official")
        .header("x-opencode-client", "cli")
        .header("x-opencode-request", "req_official")
        .header("x-opencode-project", "proj_official")
        .json(&serde_json::json!({
            "model": "mimo-v2.5-free",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(official.status(), StatusCode::OK);

    let foreign = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .header(reqwest::header::USER_AGENT, "Cursor/1.0")
        .header("x-session-id", "ses_from_custom_provider")
        .json(&serde_json::json!({
            "model": "mimo-v2.5-free",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::OK);

    let calls = h.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].opencode_session.as_deref(), Some("ses_official"));
    assert_eq!(calls[0].opencode_client.as_deref(), Some("cli"));
    assert_eq!(calls[0].opencode_request.as_deref(), Some("req_official"));
    assert_eq!(calls[0].opencode_project.as_deref(), Some("proj_official"));
    assert_eq!(calls[0].user_agent.as_deref(), Some("opencode/1.17.7"));
    assert_eq!(
        calls[1].opencode_session.as_deref(),
        Some("ses_from_custom_provider")
    );
    assert_eq!(calls[1].opencode_client.as_deref(), Some("cli"));
    assert_eq!(calls[1].user_agent.as_deref(), Some("opencode"));
    assert_eq!(
        calls[1].opencode_project.as_deref(),
        Some("ses_from_custom_provider")
    );
}

#[tokio::test]
async fn zen_free_non_stream_success_without_usage_is_still_zero_cost_free() {
    let h = FallbackHarness::zen_go(
        &[("", &[reply(200, SUCCESS_BODY_WITHOUT_USAGE)])],
        &["normal-key"],
    )
    .await;
    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "success");
    assert_eq!(log.cost_state, "free");
    assert_eq!(log.raw_cost_usd, Some(0.0));
    assert_eq!(log.quota_debit, Some(0.0));
    assert_eq!(log.effective_paid_cost_usd, Some(0.0));
    assert_eq!((log.prompt_tokens, log.completion_tokens), (0, 0));
}

#[tokio::test]
async fn zen_free_stream_success_without_usage_is_still_zero_cost_free() {
    let h = FallbackHarness::zen_go(
        &[("", &[reply(200, CHAT_STREAM_WITHOUT_USAGE)])],
        &["normal-key"],
    )
    .await;
    let (status, body) = h.stream("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let log = h.state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "success");
    assert_eq!(log.cost_state, "free");
    assert_eq!(log.raw_cost_usd, Some(0.0));
    assert_eq!(log.quota_debit, Some(0.0));
    assert_eq!(log.effective_paid_cost_usd, Some(0.0));
    assert_eq!((log.prompt_tokens, log.completion_tokens), (0, 0));
}

#[tokio::test]
async fn zen_free_401_and_403_stop_without_touching_a_normal_credential() {
    let h = FallbackHarness::zen_go(
        &[
            (
                "",
                &[
                    reply(401, r#"{"error":{"message":"anonymous route disabled"}}"#),
                    reply(403, r#"{"error":{"message":"anonymous route forbidden"}}"#),
                ],
            ),
            ("normal-key", &[ok()]),
        ],
        &["normal-key"],
    )
    .await;

    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(
        body.to_string().contains("anonymous route disabled"),
        "{body}"
    );

    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5-free").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.to_string().contains("403"), "{body}");
    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(captured.len(), 2);
    assert!(captured.iter().all(|call| call.key.is_empty()));
    assert!(h.account("acct-1").auth_error.is_none());
}

#[tokio::test]
async fn ordered_zen_candidate_429_falls_through_to_the_next_normal_card() {
    let h = FallbackHarness::zen_go(
        &[("", &[limited()]), ("normal-key", &[ok()])],
        &["normal-key"],
    )
    .await;

    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5").await;
    assert_eq!(status, 200, "{body}");
    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(
        captured
            .iter()
            .map(|call| call.key.as_str())
            .collect::<Vec<_>>(),
        ["", "normal-key"]
    );
    assert!(captured[0].body.contains("mimo-v2.5-free"));
    assert!(captured[1].body.contains("mimo-v2.5"));
    assert!(!captured[1].body.contains("mimo-v2.5-free"));
    let logs = h.logs();
    assert_eq!(logs.len(), 2);
    assert!(logs.iter().any(|log| {
        log.route_account_id.as_deref() == Some(ZEN_FREE_ACCOUNT_ID) && log.http_status == Some(429)
    }));
    assert!(logs.iter().any(|log| {
        log.route_account_id.as_deref() == Some("acct-1") && log.status == "success"
    }));
}

#[tokio::test]
async fn shared_alias_strict_priority_follows_the_persisted_card_order() {
    let p =
        PreparedFallback::zen_go(&[("", &[ok()]), ("normal-key", &[ok()])], &["normal-key"]).await;
    p.state
        .db
        .lock()
        .reorder_accounts(&["acct-1".into(), ZEN_FREE_ACCOUNT_ID.into()])
        .unwrap();
    let h = p.bind().await;
    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5").await;
    assert_eq!(status, 200, "{body}");
    h.state
        .db
        .lock()
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into(), "acct-1".into()])
        .unwrap();
    let (status, body) = h.protocol("/v1/chat/completions", "mimo-v2.5").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(h.call_keys(), ["normal-key", ""]);
}

#[tokio::test]
async fn goat_loopback_adapter_routes_all_client_formats_with_its_own_auth_contract() {
    let (h, goat_id) = start_goat(
        &[("goat-key", &[ok(), ok(), ok(), ok()])],
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
        true,
    )
    .await;
    let goat_account = h.account(&goat_id);
    assert!(h.state.provider_contracts().production_protocol_allowed(
        &goat_account,
        COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
    ));

    for path in ["/v1/chat/completions", "/v1/responses", "/v1/messages"] {
        let (status, body) = h
            .protocol(path, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
            .await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(
            body["model"].as_str(),
            Some(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
            "{path}: {body}"
        );
    }
    let (status, body) = gemini_call(h.port, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(captured.len(), 4);
    assert!(
        captured
            .iter()
            .all(|call| call.authorization.as_deref() == Some("Bearer goat-key"))
    );
    assert!(captured.iter().all(|call| call.x_api_key.is_none()));
    assert!(
        captured
            .iter()
            .all(|call| call.path == "/provider/v1/chat/completions"),
        "{:?}",
        captured
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        captured
            .iter()
            .all(|call| !call.path.contains("/responses") && !call.path.contains("/messages")),
        "GOAT must not emit /responses or /messages: {:?}",
        captured
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(captured.iter().all(|call| {
        call.body
            .contains(COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
            && !call.body.contains("\"x-cmdc-zdr\"")
    }));
    let logs = h.logs();
    assert!(logs.iter().all(|log| {
        log.route_account_id.as_deref() == Some(goat_id.as_str())
            && log.provider_id.as_deref() == Some(COMMAND_CODE_PROVIDER_ID)
            && log.credential_account_id.as_deref() == Some(goat_id.as_str())
            && log.model == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
    }));
    assert!(logs.iter().all(|log| {
        log.status == "success_unpriced"
            && log.cost_state == "unpriced"
            && log.cost.is_none()
            && log.raw_cost_usd.is_none()
            && log.quota_debit.is_none()
            && log.effective_paid_cost_usd.is_none()
            && log.pricing_revision_id.is_none()
            && log.quota_multiplier.is_none()
            && log.local_adjustment_multiplier.is_none()
    }));
}

#[tokio::test]
async fn disabled_goat_protocol_fails_locally_without_upstream() {
    let p = PreparedFallback::go(&[("goat-key", &[ok()])], &["open-key"]).await;
    let origin = p.base_url.clone();
    let goat_id = prepare_goat(
        &p.state,
        "goat-key",
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
    );
    p.state
        .db
        .lock()
        .set_model_protocol_overrides(
            &ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
            &[(
                COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM.into(),
                ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
                ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
            )],
            Utc::now(),
        )
        .unwrap();
    p.state.reload_provider_contracts().unwrap();
    let mut h = p.bind().await;
    h.attach_goat_route(goat_id, origin);

    let (status, body) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "disabled GOAT protocol must fail before sending its stored Key upstream"
    );
}

#[tokio::test]
async fn goat_preset_alias_routes_before_go_when_account_order_prefers_goat() {
    let (h, _) = start_goat(&[("goat-key", &[ok()])], &[], true, true).await;
    let (status, body) = h.chat().await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(h.call_keys(), ["goat-key"]);
}

#[tokio::test]
async fn mixed_goat_cooldown_and_sticky_state_are_independent() {
    let p = PreparedFallback::start(
        script(&[("goat-key", &[limited()]), ("open-key", &[ok()])]),
        &["open-key"],
        RoutingMode::StickyGlobal,
        false,
        false,
    )
    .await;
    let origin = p.base_url.clone();
    let goat_id = prepare_goat(
        &p.state,
        "goat-key",
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
    );
    let mut h = p.bind().await;
    h.attach_goat_route(goat_id.clone(), origin);

    let (status, body) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "pinned GOAT 429 must not fall through to Go: {body}"
    );
    assert_eq!(h.call_keys(), ["goat-key"]);
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.path == "/provider/v1/chat/completions")
    );
    let goat = h.account(&goat_id);
    let open = h.account("acct-1");
    assert!(goat.cooldown_until.is_some());
    assert!(goat.cooldown_generic_until.is_some());
    assert!(goat.cooldown_5h_until.is_none());
    assert!(goat.cooldown_week_until.is_none());
    assert!(goat.cooldown_month_until.is_none());
    assert!(open.cooldown_until.is_none());
    let sync = h
        .state
        .db
        .lock()
        .account_usage_sync_state(&goat_id)
        .unwrap();
    assert!(
        sync.as_ref()
            .is_none_or(|state| state.next_eligible_at.is_none()),
        "GOAT 429 must not schedule OpenCode Go usage sync: {sync:?}"
    );
}

#[tokio::test]
async fn goat_plan_window_429_persists_the_exact_weekly_deadline() {
    let expected_reset = Utc::now() + Duration::hours(2);
    let reset_text = expected_reset.to_rfc3339_opts(SecondsFormat::Millis, true);
    let body: &'static str = Box::leak(
        format!(
            r#"{{"error":{{"code":"RATE_LIMITED","message":"You've reached your weekly usage limit for your plan. Your limit resets at {reset_text}. Please wait for the window to reset or upgrade your plan to continue.","type":"rate_limit_error"}}}}"#
        )
        .into_boxed_str(),
    );
    let replies = [reply(StatusCode::TOO_MANY_REQUESTS.as_u16(), body)];
    let entries = [("goat-key", replies.as_slice())];
    let (h, goat_id) = start_goat(
        &entries,
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
        true,
    )
    .await;

    let (status, response) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        )
        .await;
    assert_ne!(status, StatusCode::OK, "{response}");

    let goat = h.account(&goat_id);
    assert!(goat.cooldown_generic_until.is_none());
    assert!(goat.cooldown_5h_until.is_none());
    assert_eq!(
        goat.cooldown_week_until
            .map(|deadline| deadline.timestamp_millis()),
        Some(expected_reset.timestamp_millis())
    );
    assert_eq!(
        goat.cooldown_until
            .map(|deadline| deadline.timestamp_millis()),
        Some(expected_reset.timestamp_millis())
    );
}

async fn v3_post(
    port: u16,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/dashboard/api/v3{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let parsed = response.json().await.unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

fn v3_cas(state: &CoreStateInner, patch: serde_json::Value) -> serde_json::Value {
    let mut body = patch.as_object().cloned().unwrap_or_default();
    body.insert(
        "expectedRevision".into(),
        serde_json::json!(state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        serde_json::json!(state.process_generation()),
    );
    serde_json::Value::Object(body)
}

#[tokio::test]
async fn dynamic_429_uses_generic_cooldown_skips_go_windows_and_falls_through() {
    let p =
        PreparedFallback::go(&[("dyn-first", &[limited()]), ("dyn-second", &[ok()])], &[]).await;
    let origin = p.base_url.clone();
    let h = p.bind().await;
    let (status, created) = v3_post(
        h.port,
        "/providers",
        v3_cas(
            &h.state,
            serde_json::json!({
                "name": "Lab",
                "endpointUrl": origin,
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "dyn-first",
                "models": [{
                    "publicModel": "lab-opus",
                    "upstreamModel": "vendor/opus"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let first_id = h
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .expect("dynamic provider creates a first account");
    let (status, second) = v3_post(
        h.port,
        "/accounts",
        v3_cas(
            &h.state,
            serde_json::json!({
                "name": "second",
                "providerId": provider_id,
                "key": "dyn-second"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_id = second["account"]["id"].as_str().unwrap().to_string();
    h.set_enabled(&first_id, true);
    h.set_enabled(&second_id, true);

    let (status, body) = h.protocol("/v1/chat/completions", "lab-opus").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys(), ["dyn-first", "dyn-second"]);
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.path == "/v1/chat/completions")
    );
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.body.contains("vendor/opus")),
        "{:?}",
        h.calls.lock().unwrap()
    );
    let first = h.account(&first_id);
    let second = h.account(&second_id);
    assert!(first.cooldown_until.is_some());
    assert!(first.cooldown_generic_until.is_some());
    assert!(first.cooldown_5h_until.is_none());
    assert!(first.cooldown_week_until.is_none());
    assert!(first.cooldown_month_until.is_none());
    assert!(second.cooldown_until.is_none());
    let sync = h
        .state
        .db
        .lock()
        .account_usage_sync_state(&first_id)
        .unwrap();
    assert!(
        sync.as_ref()
            .is_none_or(|state| state.next_eligible_at.is_none()),
        "dynamic 429 must not schedule OpenCode Go usage sync: {sync:?}"
    );
}

#[tokio::test]
async fn shared_alias_respects_account_order_and_can_prefer_go() {
    let (h, _) = start_goat(&[("open-key", &[ok()])], &[], false, true).await;
    let (status, body) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys(), ["open-key"]);
    assert!(
        h.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call.path == "/v1/chat/completions")
    );
}

#[tokio::test]
async fn enabled_goat_without_loopback_is_not_selected() {
    let (h, _) = start_goat(&[("open-key", &[ok()])], &[], true, false).await;
    let (status, body) = h
        .protocol(
            "/v1/chat/completions",
            COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM,
        )
        .await;
    assert_ne!(
        status,
        StatusCode::OK,
        "enabled but unverified GOAT must stay unselected: {body}"
    );
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "pinned GOAT raw id must not fall through to Go: {:?}",
        h.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn goat_only_anthropic_model_stays_raw_and_converts_client_responses() {
    let (h, _) = start_goat(
        &[("goat-key", &[ok_messages(), ok_messages()])],
        &["claude-sonnet-4-6"],
        true,
        true,
    )
    .await;

    let models = loopback_client()
        .get(format!("http://127.0.0.1:{}/v1/models", h.port))
        .bearer_auth("gw-test")
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);
    let models: serde_json::Value = models.json().await.unwrap();
    let ids = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    assert!(
        !ids.iter().any(|id| id == "claude-sonnet-4-6"),
        "a GOAT-only ID must not expand the static Go Alias namespace: {ids:?}"
    );
    assert!(
        !ids.iter()
            .any(|id| id == COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
        "GOAT raw IDs must stay unpublished: {ids:?}"
    );

    for path in ["/v1/messages", "/v1/responses"] {
        let (status, body) = h.protocol(path, "claude-sonnet-4-6").await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(captured.len(), 2, "{captured:?}");
    assert!(
        captured
            .iter()
            .all(|call| call.authorization.as_deref() == Some("Bearer goat-key"))
    );
    h.state
        .db
        .lock()
        .set_model_protocol_overrides(
            &ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
            &[(
                "claude-sonnet-4-6".into(),
                ocg_core::provider::UpstreamProtocolKind::Messages,
                ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
            )],
            Utc::now(),
        )
        .unwrap();
    h.state.reload_provider_contracts().unwrap();
    let models = loopback_client()
        .get(format!("http://127.0.0.1:{}/v1/models", h.port))
        .bearer_auth("gw-test")
        .send()
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);
    let models: serde_json::Value = models.json().await.unwrap();
    let ids = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !ids.contains(&"claude-sonnet-4-6"),
        "GOAT-only raw model must remain absent from /v1/models: {ids:?}"
    );
    assert!(
        captured
            .iter()
            .all(|call| call.path == "/provider/v1/messages"),
        "{:?}",
        captured
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>()
    );
    assert!(captured.iter().all(|call| call.x_api_key.is_none()));
}

#[tokio::test]
async fn sticky_global_keeps_failover_account_after_higher_priority_recovers() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;

    assert_eq!(h.chat().await.0, 200);
    h.set_enabled("acct-1", false);
    assert_eq!(h.chat().await.0, 200);
    h.set_enabled("acct-1", true);
    assert_eq!(h.chat().await.0, 200);
    assert_eq!(h.call_keys(), ["key-1", "key-2", "key-2"]);
}

#[tokio::test]
async fn round_robin_cycles_and_skips_a_disabled_account() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
        RoutingMode::RoundRobin,
        false,
    )
    .await;

    assert_eq!(h.chat().await.0, 200);
    h.set_enabled("acct-2", false);
    assert_eq!(h.chat().await.0, 200);
    h.set_enabled("acct-2", true);
    assert_eq!(h.chat().await.0, 200);
    assert_eq!(h.chat().await.0, 200);
    assert_eq!(h.call_keys(), ["key-1", "key-1", "key-2", "key-1"]);
}

#[tokio::test]
async fn explicit_conversation_bindings_are_sticky_and_private() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
        RoutingMode::RoundRobin,
        true,
    )
    .await;

    for (conversation, user) in [
        ("conversation-a", "a1"),
        ("conversation-b", "b1"),
        ("conversation-a", "a2"),
        ("conversation-b", "b2"),
    ] {
        assert_eq!(
            h.chat_with_conversation(Some(conversation), user).await.0,
            200
        );
    }

    let calls = h.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| call.key.as_str())
            .collect::<Vec<_>>(),
        ["key-1", "key-2", "key-1", "key-2"]
    );
    assert!(calls.iter().all(|call| call.conversation_header.is_none()));
    let sessions = calls
        .iter()
        .map(|call| call.opencode_session.as_deref().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sessions[0], sessions[2]);
    assert_eq!(sessions[1], sessions[3]);
    assert_ne!(sessions[0], sessions[1]);
}

#[tokio::test]
async fn explicit_opencode_session_is_preserved_for_go_and_zen_free() {
    let go = FallbackHarness::go(&[("key-1", &[ok()])], &["key-1"]).await;
    let zen = FallbackHarness::zen_go(&[("", &[ok()]), ("key-1", &[ok()])], &["key-1"]).await;
    let (goat, _) = start_goat(
        &[("goat-key", &[ok()])],
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
        true,
    )
    .await;
    for (h, model, expected) in [
        (&go, "deepseek-v4-flash", Some("private-go-session")),
        (&zen, "mimo-v2.5-free", Some("private-go-session")),
        (&goat, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, None),
    ] {
        let response = loopback_client()
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
            .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
            .header("x-opencode-session", "private-go-session")
            .header("x-session-id", "lower-priority-session")
            .json(&serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
        assert_eq!(
            h.calls.lock().unwrap()[0].opencode_session.as_deref(),
            expected
        );
    }
}

#[tokio::test]
async fn opencode_identity_headers_are_go_zen_only_and_preserve_explicit_values() {
    let go = FallbackHarness::go(&[("key-1", &[ok()])], &["key-1"]).await;
    let zen = FallbackHarness::zen_go(&[("", &[ok()]), ("key-1", &[ok()])], &["key-1"]).await;
    let (goat, _) = start_goat(
        &[("goat-key", &[ok()])],
        &[COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM],
        true,
        true,
    )
    .await;

    for (h, model) in [
        (&go, "deepseek-v4-flash"),
        (&zen, "mimo-v2.5-free"),
        (&goat, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM),
    ] {
        let response = loopback_client()
            .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
            .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
            .header("x-opencode-session", "ses_explicit")
            .header("x-opencode-client", "desktop")
            .header("x-opencode-request", "req_explicit")
            .header("x-opencode-project", "proj_explicit")
            .header("x-session-id", "ses_id_explicit")
            .header("x-session-affinity", "ses_aff_explicit")
            .json(&serde_json::json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            response.text().await.unwrap()
        );
    }

    let go_call = go.calls.lock().unwrap()[0].clone();
    assert_eq!(go_call.opencode_session.as_deref(), Some("ses_explicit"));
    assert_eq!(go_call.opencode_client.as_deref(), Some("desktop"));
    assert_eq!(go_call.opencode_request.as_deref(), Some("req_explicit"));
    assert_eq!(go_call.opencode_project.as_deref(), Some("proj_explicit"));
    assert!(go_call.session_id.is_none(), "{go_call:?}");
    assert!(go_call.session_affinity.is_none(), "{go_call:?}");

    let zen_call = zen.calls.lock().unwrap()[0].clone();
    assert_eq!(zen_call.opencode_session.as_deref(), Some("ses_explicit"));
    assert_eq!(zen_call.opencode_client.as_deref(), Some("desktop"));
    assert_eq!(zen_call.opencode_request.as_deref(), Some("req_explicit"));
    assert_eq!(zen_call.opencode_project.as_deref(), Some("proj_explicit"));
    assert!(zen_call.session_id.is_none(), "{zen_call:?}");
    assert!(zen_call.session_affinity.is_none(), "{zen_call:?}");

    let goat_call = goat.calls.lock().unwrap()[0].clone();
    assert!(goat_call.opencode_session.is_none(), "{goat_call:?}");
    assert!(goat_call.opencode_client.is_none(), "{goat_call:?}");
    assert!(goat_call.opencode_request.is_none(), "{goat_call:?}");
    assert!(goat_call.opencode_project.is_none(), "{goat_call:?}");
    assert!(goat_call.session_id.is_none(), "{goat_call:?}");
    assert!(goat_call.session_affinity.is_none(), "{goat_call:?}");
}

#[tokio::test]
async fn client_session_id_is_forwarded_as_the_opencode_go_session() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()])],
        &["key-1"],
        RoutingMode::StrictPriority,
        false,
    )
    .await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .header("x-session-id", "ses_from_opencode_client")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let call = h.calls.lock().unwrap()[0].clone();
    assert_eq!(
        call.opencode_session.as_deref(),
        Some("ses_from_opencode_client")
    );
    assert!(
        call.opencode_client.is_none(),
        "Go must not synthesize Zen Free client identity: {:?}",
        call.opencode_client
    );
    assert_ne!(call.user_agent.as_deref(), Some("opencode"));
}

#[tokio::test]
async fn conversation_failover_rebinds_to_the_successful_account() {
    let h = FallbackHarness::routing(
        &[
            (
                "key-1",
                &[reply(403, r#"{"error":{"message":"forbidden key"}}"#)],
            ),
            ("key-2", &[ok()]),
        ],
        &["key-1", "key-2"],
        RoutingMode::StrictPriority,
        true,
    )
    .await;

    assert_eq!(
        h.chat_with_conversation(Some("conversation-rebind"), "first")
            .await
            .0,
        200
    );
    assert_eq!(
        h.chat_with_conversation(Some("conversation-rebind"), "second")
            .await
            .0,
        200
    );
    assert_eq!(h.call_keys(), ["key-1", "key-2", "key-2"]);
}

#[tokio::test]
async fn model_discovery_does_not_advance_round_robin_generation_cursor() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
        RoutingMode::RoundRobin,
        false,
    )
    .await;

    assert_eq!(h.chat().await.0, 200);
    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_local_openai_alias_list(&h.state, &body);
    assert_eq!(h.chat().await.0, 200);

    let calls = h.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|call| (call.key.as_str(), call.path.as_str()))
            .collect::<Vec<_>>(),
        [
            ("key-1", "/v1/chat/completions"),
            ("key-2", "/v1/chat/completions"),
        ]
    );
    drop(calls);
    assert_eq!(h.state.db.lock().list_forward_logs(10).unwrap().len(), 2);
}

#[tokio::test]
async fn concurrent_round_robin_requests_are_evenly_distributed() {
    let h = FallbackHarness::routing(
        &[("key-1", &[ok()]), ("key-2", &[ok()])],
        &["key-1", "key-2"],
        RoutingMode::RoundRobin,
        false,
    )
    .await;

    let requests = (0..20)
        .map(|_| {
            let port = h.port;
            tokio::spawn(async move { chat(port).await })
        })
        .collect::<Vec<_>>();
    for request in requests {
        assert_eq!(request.await.unwrap().0, 200);
    }

    let calls = h.calls.lock().unwrap();
    assert_eq!(calls.len(), 20);
    assert_eq!(calls.iter().filter(|call| call.key == "key-1").count(), 10);
    assert_eq!(calls.iter().filter(|call| call.key == "key-2").count(), 10);
}

#[tokio::test]
async fn dashboard_port_change_rebinds_and_persists_across_restart() {
    let (state, dir) = build_state("http://127.0.0.1:1".into(), &[]);
    let handle = gateway::start_gateway(state.clone(), free_port())
        .await
        .unwrap();
    let current_port = handle.port;
    *state.gateway.lock() = Some(handle);
    {
        let mut config = state.config();
        config.gateway_port = current_port;
        state.set_config(config).unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let requested_port = free_port();
    assert_ne!(
        requested_port, current_port,
        "the settings write must request a different port than the live listener"
    );
    let settings_payload = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "gatewayPort": requested_port
    });
    let client = loopback_client();
    let response = client
        .put(format!(
            "http://127.0.0.1:{current_port}/dashboard/api/v3/settings"
        ))
        .json(&settings_payload)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = response.json().await.unwrap();
    assert_eq!(result["revision"].as_u64(), Some(state.settings_revision()));
    assert_eq!(state.config().gateway_port, requested_port);
    assert_eq!(
        state.active_gateway_port(),
        requested_port,
        "successful HTTP port mutation rebinds the managed listener"
    );
    let stored = state.db.lock().get_setting("config").unwrap().unwrap();
    let stored: AppConfig = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored.gateway_port, requested_port);

    let status_response = client
        .get(format!(
            "http://127.0.0.1:{requested_port}/dashboard/api/v3/gateway/status"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(status_response.status(), StatusCode::OK);
    let status: serde_json::Value = status_response.json().await.unwrap();
    assert_eq!(status["running"], true);
    assert_eq!(status["port"].as_u64(), Some(u64::from(requested_port)));

    let occupied = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let fail_payload = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "gatewayPort": occupied_port
    });
    let fail = client
        .put(format!(
            "http://127.0.0.1:{requested_port}/dashboard/api/v3/settings"
        ))
        .json(&fail_payload)
        .send()
        .await
        .unwrap();
    assert_eq!(fail.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        state.active_gateway_port(),
        requested_port,
        "failed rebind must keep the live listener"
    );
    assert_eq!(
        state.config().gateway_port,
        requested_port,
        "failed rebind compensation must restore the last successful port"
    );

    let handle = state.gateway.lock().take().unwrap();
    gateway::stop_gateway_and_wait(handle).await;
    drop(state);

    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let restarted =
        CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap();
    assert_eq!(
        restarted.config().gateway_port,
        requested_port,
        "the last successful port must load on the next process start"
    );
    drop(occupied);
    drop(restarted);
    let _ = fs::remove_dir_all(dir);
}

#[tokio::test]
async fn forwarded_requests_are_attributed_to_the_authenticating_key() {
    let h = FallbackHarness::go(
        &[(
            "key-1",
            &[
                reply(
                    200,
                    r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                ),
                reply(
                    200,
                    r#"{"id":"y","choices":[{"message":{"role":"assistant","content":"yo"}}],"usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}}"#,
                ),
            ],
        )],
        &["key-1"],
    )
    .await;

    let secondary = ocg_core::gateway_keys::create_sub_key(&h.state, "Laptop").unwrap();
    let client = loopback_client();
    let body = serde_json::json!({
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 3,
        "stream": false
    });
    let secondary_status = client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", secondary.key),
        )
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(secondary_status, StatusCode::OK);

    let primary_status = h.chat().await.0;
    assert_eq!(primary_status, StatusCode::OK);

    let unauthorized_status = client
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(reqwest::header::AUTHORIZATION, "Bearer unknown-key")
        .json(&body)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(unauthorized_status, StatusCode::UNAUTHORIZED);
    assert_eq!(h.call_count(), 2, "only authenticated requests forward");

    let primary_id = ocg_core::gateway_keys::PRIMARY_KEY_ID;
    let logs = h.logs();
    assert_eq!(
        logs.len(),
        2,
        "unauthenticated requests write no forward rows"
    );
    let secondary_rows = logs
        .iter()
        .filter(|log| log.client_key_id.as_deref() == Some(secondary.id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(secondary_rows.len(), 1);
    assert_eq!(
        secondary_rows[0].client_key_name.as_deref(),
        Some("Laptop"),
        "the write-time name snapshot rides along for later renames"
    );
    let primary_rows = logs
        .iter()
        .filter(|log| log.client_key_id.as_deref() == Some(primary_id))
        .collect::<Vec<_>>();
    assert_eq!(primary_rows.len(), 1);

    let page = h
        .state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 10,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,

            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: Some(secondary.id.as_str()),
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap();
    assert_eq!(page.summary.total_requests, 1);
    assert_eq!(page.summary.prompt_tokens, 1);
    assert!(
        page.items
            .iter()
            .all(|log| log.client_key_id == Some(secondary.id.clone()))
    );
}

#[tokio::test]
async fn gateway_stays_available_while_large_backfill_runs() {
    let p = PreparedFallback::go(
        &[(
            "key-1",
            &[reply(
                200,
                r#"{"id":"x","choices":[{"message":{"role":"assistant","content":"hi"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
            )],
        )],
        &["key-1"],
    )
    .await;
    {
        let seed_rows = vec![
            legacy_forward_log();
            (ocg_core::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS + 5_000) as usize
        ];
        let db = p.state.db.lock();
        db.log_forward_batch(&seed_rows).unwrap();
        assert_eq!(
            db.forward_log_backfill_marker().unwrap(),
            None,
            "seeding must not run the backfill"
        );
    }
    let h = p.bind().await;

    let (status, _body) = h.chat().await;
    assert_eq!(status, StatusCode::OK);
    let unauthorized = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/chat/completions", h.port))
        .header(reqwest::header::AUTHORIZATION, "Bearer wrong-key")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "x"}],
            "max_tokens": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let mut marker = None;
    for _ in 0..600 {
        marker = h.state.db.lock().forward_log_backfill_marker().unwrap();
        if marker.as_deref() == Some(ocg_core::db::BACKFILL_DONE) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        marker.as_deref(),
        Some(ocg_core::db::BACKFILL_DONE),
        "backfill must complete after the seeded rows"
    );

    let unattributed: i64 = h
        .state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 1,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,

            route_account_id: None,
            credential_account_id: None,
            model: None,
            key_id: Some(ocg_core::models::UNATTRIBUTED_KEY_FILTER),
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap()
        .summary
        .total_requests;
    assert_eq!(unattributed, 0);
    let attributed_chat: i64 = h
        .state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: 1,
            offset: 0,
            status: None,
            account_id: None,
            provider_id: None,

            route_account_id: None,
            credential_account_id: None,
            model: Some("deepseek-v4-flash"),
            key_id: None,
            request_id: None,
            start_time: None,
            end_time: None,
            sort_by: None,
            sort_order: None,
        })
        .unwrap()
        .summary
        .total_requests;
    assert_eq!(attributed_chat, 1);
}

#[derive(Clone)]
struct SwitchingProxyState {
    state: Arc<ocg_core::state::CoreStateInner>,
    replies: Arc<Mutex<VecDeque<MockReply>>>,
    switched: Arc<AtomicBool>,
}

async fn switching_proxy_chat(
    axum::extract::State(server): axum::extract::State<SwitchingProxyState>,
) -> impl IntoResponse {
    if !server.switched.swap(true, Ordering::SeqCst) {
        let mut config = server.state.config();
        config.proxy_mode = ProxyMode::Direct;
        server.state.set_config(config).unwrap();
    }
    let reply = server
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("switching proxy replies must be pre-seeded");
    (
        StatusCode::from_u16(reply.status).unwrap(),
        [("content-type", "application/json")],
        reply.body,
    )
}

#[tokio::test]
async fn list_mode_routes_listed_models_through_the_proxy_leg_and_labels_logs() {
    let (upstream_base, upstream_calls, stop_upstream) =
        start_fake_upstream(script(&[("key-1", &[ok()])])).await;
    let (proxy_base, proxy_calls, stop_proxy) =
        start_fake_upstream(script(&[("key-1", &[ok_responses()])])).await;
    let (state, dir) = build_state(upstream_base.clone(), &["key-1"]);
    apply_list_whitelist_config(&state, upstream_base, &proxy_base, &["gpt-5.6-luna"]);
    let mut h = FallbackHarness::from_state(state, dir).await;
    h.push_stop(stop_upstream);
    h.push_stop(stop_proxy);

    let (status, _) = h.protocol("/v1/chat/completions", "gpt-5.6-luna").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h.protocol("/v1/chat/completions", "glm-5.2").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        proxy_calls.lock().unwrap().len(),
        1,
        "listed model must traverse the proxy leg"
    );
    assert_eq!(
        upstream_calls.lock().unwrap().len(),
        1,
        "unlisted model must connect directly"
    );

    let logs = h.logs();
    let luna = logs
        .iter()
        .find(|log| log.model == "gpt-5.6-luna")
        .expect("listed model row");
    assert_eq!(luna.route, "proxy");
    let glm = logs
        .iter()
        .find(|log| log.model == "glm-5.2")
        .expect("unlisted model row");
    assert_eq!(glm.route, "direct");
}

#[tokio::test]
async fn list_mode_free_fallback_reroutes_to_the_default_leg_mid_request() {
    let (upstream_base, upstream_calls, stop_upstream) =
        start_fake_upstream(script(&[("key-1", &[ok()])])).await;
    let (proxy_base, proxy_calls, stop_proxy) =
        start_fake_upstream(script(&[("", &[limited()])])).await;
    let (state, dir) = build_state(format!("{upstream_base}/zen/go"), &["key-1"]);
    apply_list_whitelist_config(
        &state,
        format!("{upstream_base}/zen/go"),
        &proxy_base,
        &["mimo-v2.5-free"],
    );
    state
        .db
        .lock()
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into(), "acct-1".into()])
        .unwrap();
    let mut h = FallbackHarness::from_state(state, dir).await;
    h.push_stop(stop_upstream);
    h.push_stop(stop_proxy);

    let (status, _) = h.protocol("/v1/chat/completions", "mimo-v2.5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        proxy_calls.lock().unwrap().len(),
        1,
        "the free twin attempt must use the listed proxy leg"
    );
    assert_eq!(
        upstream_calls.lock().unwrap().len(),
        1,
        "the Go fallback must use the direct default leg"
    );

    let logs = h.logs();
    let free_row = logs
        .iter()
        .find(|log| log.model == "mimo-v2.5-free")
        .expect("free attempt row");
    assert_eq!(
        free_row.route, "proxy",
        "free failure rows carry the leg too"
    );
    let go_row = logs
        .iter()
        .find(|log| log.model == "mimo-v2.5" && log.status == "success")
        .expect("Go fallback success row");
    assert_eq!(go_row.route, "direct");
}

#[tokio::test]
async fn list_mode_midflight_config_switch_keeps_the_entry_snapshot() {
    let (upstream_base, upstream_calls, stop_upstream) = start_fake_upstream(HashMap::new()).await;
    let replies = Arc::new(Mutex::new(VecDeque::from([
        MockReply {
            status: 403,
            body: r#"{"error":"first attempt rejected, rotate to next account"}"#,
        },
        MockReply {
            status: 200,
            body: RESPONSES_SUCCESS_BODY,
        },
    ])));
    let switched = Arc::new(AtomicBool::new(false));
    let (state, dir) = build_state(upstream_base.clone(), &["key-1", "key-2"]);
    let proxy_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let proxy_base = format!("http://{}", proxy_listener.local_addr().unwrap());
    let (proxy_shutdown_tx, proxy_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let proxy_app = Router::new()
        .fallback(switching_proxy_chat)
        .with_state(SwitchingProxyState {
            state: state.clone(),
            replies: replies.clone(),
            switched: switched.clone(),
        });
    tokio::spawn(async move {
        let server = axum::serve(proxy_listener, proxy_app).with_graceful_shutdown(async move {
            let _ = proxy_shutdown_rx.await;
        });
        let _ = server.await;
    });
    apply_list_whitelist_config(&state, upstream_base, &proxy_base, &["gpt-5.6-luna"]);
    let mut h = FallbackHarness::from_state(state, dir).await;
    h.push_stop(stop_upstream);
    h.push_stop(proxy_shutdown_tx);

    let (status, _) = h.protocol("/v1/chat/completions", "gpt-5.6-luna").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        switched.load(Ordering::SeqCst),
        "config must have flipped mid-flight"
    );
    assert_eq!(
        replies.lock().unwrap().len(),
        0,
        "both attempts must have hit the proxy leg of the entry snapshot"
    );
    assert_eq!(
        upstream_calls.lock().unwrap().len(),
        0,
        "the in-flight request must not observe the Direct switch"
    );
    assert_eq!(h.state.config().proxy_mode, ProxyMode::Direct);
    assert!(
        h.logs()
            .iter()
            .filter(|log| log.model == "gpt-5.6-luna")
            .all(|log| log.route == "proxy")
    );
}

#[tokio::test]
async fn disabled_protocols_fail_locally_without_upstream() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    disable_go_protocols(&p.state, "glm-5.3", false, false, false);
    disable_command_protocols(&p.state, "zai-org/GLM-5.3");
    let h = p.bind().await;

    let (status, body) = h.protocol("/v1/chat/completions", "glm-5.3").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string()
            .contains(ocg_core::provider_contracts::NO_ENABLED_UPSTREAM_PROTOCOL)
            || body.to_string().contains("no enabled upstream"),
        "{body}"
    );
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "disabled protocols must fail before upstream: {:?}",
        h.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn protocol_switch_filters_v1_models_and_application_models() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    persist_goat_verified_catalog(&p.state, "catalog-only", &["zai-org/GLM-5.3"]);
    disable_go_protocols(&p.state, "glm-5.3", false, true, true);
    let h = p.bind().await;

    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("\"glm-5.3\""),
        "glm-5.3 must stay while Command Code still supplies the shared Alias: {body}"
    );
    assert!(
        body.contains("\"grok-4.5\""),
        "responses-only grok-4.5 must remain: {body}"
    );

    let (status, app_body) = h.application_models().await;
    assert_eq!(status, StatusCode::OK, "{app_body}");
    let ids = application_model_ids(&app_body);
    assert!(!ids.iter().any(|id| id == "glm-5.3"));
    assert!(ids.iter().any(|id| id == "grok-4.5"));

    disable_command_protocols(&h.state, "zai-org/GLM-5.3");
    let (status, body) = h.models().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !body.contains("\"glm-5.3\""),
        "the Alias must leave /v1/models after every Provider mapping is off: {body}"
    );
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "listing endpoints must stay local: {:?}",
        h.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn reenabling_a_protocol_restores_routing_without_a_new_probe() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    disable_go_protocols(&p.state, "glm-5.3", false, true, true);
    let glm = p
        .state
        .provider_contracts()
        .scope(&ocg_core::provider_contracts::ContractScope::provider(
            OPENCODE_PROVIDER_ID,
        ))
        .unwrap()
        .model("glm-5.3")
        .unwrap()
        .clone();
    assert!(glm.protocols.get("chat_completions").unwrap().available);
    assert!(!glm.protocols.get("chat_completions").unwrap().enabled);

    p.state
        .db
        .lock()
        .set_model_protocol_overrides(
            &ocg_core::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID),
            &[(
                "glm-5.3".into(),
                ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
                ocg_core::provider_contracts::ProtocolOverrideState::Auto,
            )],
            Utc::now(),
        )
        .unwrap();
    p.state.reload_provider_contracts().unwrap();
    let h = p.bind().await;
    let (status, body) = h.protocol("/v1/chat/completions", "glm-5.3").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_count(), 1);
}

#[tokio::test]
async fn duplicate_protocol_probes_fail_locally_without_upstream() {
    let h = FallbackHarness::go(&[("key-1", &[ok()])], &["key-1"]).await;
    let (status, body) = dashboard_protocol_probe(
        h.port,
        &h.state,
        "acct-1",
        "glm-5.2",
        &["chat_completions", "responses", "chat_completions"],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        h.calls.lock().unwrap().is_empty(),
        "a duplicated protocol must not run a billable probe: {:?}",
        h.calls.lock().unwrap()
    );
}

#[tokio::test]
async fn explicit_probe_records_results_without_changing_production_protocol() {
    let h = FallbackHarness::go(
        &[(
            "key-1",
            &[reply(500, r#"{"error":"nope"}"#), ok(), ok_responses()],
        )],
        &["key-1"],
    )
    .await;

    let before = h
        .state
        .provider_contracts()
        .scope(&ocg_core::provider_contracts::ContractScope::provider(
            OPENCODE_PROVIDER_ID,
        ))
        .unwrap()
        .model("grok-4.5")
        .unwrap()
        .clone();
    assert!(!before.protocols.get("chat_completions").unwrap().available);
    assert!(before.protocols.get("responses").unwrap().available);

    let (status, body) = dashboard_protocol_probe(
        h.port,
        &h.state,
        "acct-1",
        "grok-4.5",
        &["chat_completions"],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["success"], false);
    assert_eq!(body["results"][0]["skipped"], false);
    let after_failure = h
        .state
        .provider_contracts()
        .scope(&ocg_core::provider_contracts::ContractScope::provider(
            OPENCODE_PROVIDER_ID,
        ))
        .unwrap()
        .model("grok-4.5")
        .unwrap()
        .clone();
    assert!(
        !after_failure
            .protocols
            .get("chat_completions")
            .unwrap()
            .available
    );
    assert!(after_failure.protocols.get("responses").unwrap().available);
    assert_eq!(
        after_failure
            .protocols
            .get("chat_completions")
            .unwrap()
            .last_probe_result,
        Some(ocg_core::provider_contracts::ProbeResultKind::Failure)
    );

    let (status, body) = dashboard_protocol_probe(
        h.port,
        &h.state,
        "acct-1",
        "grok-4.5",
        &["chat_completions"],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["results"][0]["success"], true);
    let after_success = h
        .state
        .provider_contracts()
        .scope(&ocg_core::provider_contracts::ContractScope::provider(
            OPENCODE_PROVIDER_ID,
        ))
        .unwrap()
        .model("grok-4.5")
        .unwrap()
        .clone();
    assert!(
        !after_success
            .protocols
            .get("chat_completions")
            .unwrap()
            .available
    );
    assert!(
        !after_success
            .protocols
            .get("chat_completions")
            .unwrap()
            .enabled
    );
    assert_eq!(
        after_success
            .protocols
            .get("chat_completions")
            .unwrap()
            .last_probe_result,
        Some(ocg_core::provider_contracts::ProbeResultKind::Success)
    );

    let (status, body) = h.protocol("/v1/chat/completions", "grok-4.5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "ok");
    let recorded = h.calls.lock().unwrap();
    assert_eq!(
        recorded
            .iter()
            .map(|call| call.path.as_str())
            .collect::<Vec<_>>(),
        [
            "/v1/chat/completions",
            "/v1/chat/completions",
            "/v1/responses"
        ]
    );
}

#[tokio::test]
async fn cpa_fake_upstream_production_forwarding_uses_bearer_and_bypasses_global_proxy() {
    let replies = script(&[("cpa-inference-key", &[ok()])]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone(), &[]);
    let now = Utc::now();
    let account = Account {
        id: CPA_ACCOUNT_ID.to_string(),
        provider_id: CPA_PROVIDER_ID.to_string(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: CPA_ACCOUNT_NAME.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("cpa-inference-key").unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    let management = state.encrypt_key("cpa-management-key").unwrap();
    state
        .db
        .lock()
        .upsert_cpa_integration(&account, &base_url, &management)
        .unwrap();
    state
        .activate_cpa_model_catalog(vec!["cpa-forward-model".into()], &base_url, now)
        .unwrap();
    let mut config = state.config();
    config.proxy_mode = ProxyMode::Manual;
    config.proxy_url = "http://127.0.0.1:1".into();
    state.set_config(config).unwrap();

    let h = FallbackHarness::from_parts(state, dir, calls, Some(stop_mock), None).await;
    let (status, body) = h
        .protocol("/v1/chat/completions", "cpa-forward-model")
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let captured = h.calls.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].path, "/v1/chat/completions");
    assert_eq!(
        captured[0].authorization.as_deref(),
        Some("Bearer cpa-inference-key")
    );
    assert!(
        captured[0].body.contains("\"cpa-forward-model\""),
        "CPA must forward the exact catalog model: {}",
        captured[0].body
    );
    let logs = h.logs();
    assert!(
        logs.iter().any(|log| {
            log.provider_id.as_deref() == Some(CPA_PROVIDER_ID)
                && log.route_account_id.as_deref() == Some(CPA_ACCOUNT_ID)
                && log.model == "cpa-forward-model"
                && log.http_status == Some(200)
        }),
        "CPA attribution missing: {logs:?}"
    );
    h.stop();
}
