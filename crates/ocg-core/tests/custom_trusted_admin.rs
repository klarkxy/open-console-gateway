//! Black-box coverage for administrator-trusted, verified, routeable Custom API.

use ocg_core::custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "fixtures/blackbox/harness.rs"]
mod harness;

use harness::*;

const CUSTOM_MODEL: &str = "custom-local-model";
const CUSTOM_MODEL_2: &str = "custom-other-model";
const MAPPED_PUBLIC_MODEL: &str = "deepseek-v4-flash";
const MAPPED_UPSTREAM_MODEL: &str = "deepseek-v4-flash:0731";
const CHAT_PREFERRED_BUILTIN_ALIAS: &str = "hy3";
const CUSTOM_KEY_2: &str = "v2-secret-KEY-9f3a2c1b-custom-2";
const SUCCESS_RESPONSES_BODY: &str = r#"{"id":"ok","object":"response","model":"upstream-should-not-leak","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":2}}"#;
const SUCCESS_MESSAGES_BODY: &str = r#"{"id":"ok","type":"message","role":"assistant","model":"upstream-should-not-leak","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;

fn custom_origin(harness: &BlackBoxHarness) -> String {
    harness
        .upstream_base_url
        .trim_end_matches('/')
        .trim_end_matches("/zen/go")
        .to_string()
}

fn custom_endpoint(base_url: &str, protocol: &str) -> String {
    let suffix = match protocol {
        "chat_completions" => "chat/completions",
        "responses" => "responses",
        "messages" => "messages",
        other => panic!("unsupported Custom test protocol {other}"),
    };
    format!("{}/{suffix}", base_url.trim_end_matches('/'))
}

fn protocol_success_replies(
    keys: &[&str],
    body: &'static str,
) -> HashMap<String, VecDeque<FakeReply>> {
    let mut replies = HashMap::new();
    for key in keys {
        replies.insert(
            (*key).to_string(),
            VecDeque::from([FakeReply { status: 200, body }]),
        );
    }
    replies
}

async fn verify_account(harness: &BlackBoxHarness, id: &str) -> (StatusCode, Value) {
    let (status, body) = harness
        .post_json(
            &format!("/accounts/{id}/verify"),
            &json!({ "expectedRevision": harness.settings_revision().await }),
        )
        .await;
    if status.is_success() {
        (status, mutation_account(body))
    } else {
        (status, body)
    }
}

async fn toggle_account(harness: &BlackBoxHarness, id: &str) -> (StatusCode, Value) {
    let (status, body) = harness
        .post_json(
            &format!("/accounts/{id}/toggle"),
            &json!({ "expectedRevision": harness.settings_revision().await }),
        )
        .await;
    if status.is_success() {
        (status, mutation_account(body))
    } else {
        (status, body)
    }
}

async fn ensure_account_enabled(harness: &BlackBoxHarness, id: &str) {
    let enabled = harness
        .state
        .db
        .lock()
        .get_account(id)
        .unwrap()
        .expect("account")
        .enabled;
    if !enabled {
        let (status, body) = toggle_account(harness, id).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], true, "{body}");
    }
}

async fn create_verified_enabled_custom(
    harness: &BlackBoxHarness,
    name: &str,
    key: &str,
    model_id: &str,
    protocol: &str,
    _auth_scheme: &str,
) -> Value {
    let origin = custom_origin(harness);
    let endpoint = custom_endpoint(&origin, protocol);
    let (status, draft) = harness
        .create_account(json!({
            "providerId": CUSTOM_PROVIDER_ID,
            "name": name,
            "key": key,
            "expectedRevision": harness.settings_revision().await,
            "customConfig": {
                "endpointUrl": endpoint,
                "upstreamProtocol": protocol
            },
            "modelCapabilities": [{
                "modelId": model_id,
                "protocol": protocol
            }]
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(draft["enabled"], true, "{draft}");
    assert_eq!(draft["verificationStatus"].as_str(), Some("pending"));
    let id = draft["id"].as_str().unwrap().to_string();
    harness.enable_stored_account(&id);
    let (status, verified) = verify_account(harness, &id).await;
    assert_eq!(status, StatusCode::OK, "{verified}");
    assert_eq!(
        verified["enabled"], true,
        "verify must not change an enabled card: {verified}"
    );
    assert_eq!(
        verified["verificationStatus"].as_str(),
        Some("verified"),
        "{verified}"
    );
    verified
}

async fn create_verified_enabled_custom_mapping(
    harness: &BlackBoxHarness,
    name: &str,
    key: &str,
    public_model: &str,
    upstream_model: &str,
) -> Value {
    let origin = custom_origin(harness);
    let (status, draft) = harness
        .create_account(json!({
            "providerId": CUSTOM_PROVIDER_ID,
            "name": name,
            "key": key,
            "expectedRevision": harness.settings_revision().await,
            "customConfig": {
                "endpointUrl": custom_endpoint(&origin, "chat_completions"),
                "upstreamProtocol": "chat_completions"
            },
            "modelCapabilities": [{
                "publicModel": public_model,
                "upstreamModel": upstream_model,
                "protocol": "chat_completions"
            }]
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let id = draft["id"].as_str().unwrap().to_string();
    harness.enable_stored_account(&id);
    let (status, verified) = verify_account(harness, &id).await;
    assert_eq!(status, StatusCode::OK, "{verified}");
    verified
}

#[tokio::test]
async fn custom_catalog_is_routable_with_available_verification() {
    let harness = BlackBoxHarness::start().await;
    let catalog = harness.catalog().await;
    let custom = catalog_entry(&catalog, CUSTOM_PROVIDER_ID).unwrap();
    assert_eq!(custom["routable"], true, "{custom}");
    assert_eq!(
        custom["verificationRuntimeAvailability"].as_str(),
        Some("available")
    );
    assert_eq!(custom["pricingAvailability"].as_str(), Some("unpriced"));
    assert_eq!(custom["usageAvailability"].as_str(), Some("unavailable"));
    harness.shutdown();
}

#[tokio::test]
async fn verification_failure_persists_failed_without_enabling() {
    let harness = BlackBoxHarness::start().await;
    let (status, draft) = harness
        .create_account(custom_create_payload(
            "custom-fail",
            CUSTOM_ACCOUNT_KEY,
            harness.settings_revision().await,
            "http://127.0.0.1:1",
            CUSTOM_MODEL,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let id = draft["id"].as_str().unwrap().to_string();
    let (status, body) = verify_account(&harness, &id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["enabled"], true,
        "failed verify must not change the card's enabled state: {body}"
    );
    assert_eq!(
        body["verificationStatus"].as_str(),
        Some("failed"),
        "{body}"
    );
    assert!(
        body["verificationError"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "{body}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn chat_bearer_verifies_lists_resolves_and_logs_unknown_cost() {
    let harness = BlackBoxHarness::start_with_upstream(Some(protocol_success_replies(
        &[CUSTOM_ACCOUNT_KEY],
        SUCCESS_CHAT_BODY,
    )))
    .await;
    let before = harness.list_client_models().await;
    assert_eq!(before.0, StatusCode::OK);
    let before_ids: Vec<String> = before.1["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();

    let account = create_verified_enabled_custom(
        &harness,
        "custom-chat",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
    )
    .await;

    let after = harness.list_client_models().await.1;
    let after_ids: Vec<String> = after["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    assert_eq!(&after_ids[..before_ids.len()], before_ids.as_slice());
    assert!(after_ids.contains(&CUSTOM_MODEL.to_string()), "{after}");
    assert!(
        after["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == CUSTOM_MODEL && item["owned_by"] == CUSTOM_PROVIDER_ID)
    );

    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"].as_str(), Some(CUSTOM_MODEL));
    let calls = harness.fake_calls();
    assert_eq!(
        calls.last().map(|call| call.key.as_str()),
        Some(CUSTOM_ACCOUNT_KEY)
    );
    assert!(
        calls.iter().any(|call| {
            call.path.ends_with("/chat/completions")
                && call.authorization.as_deref() == Some(&format!("Bearer {CUSTOM_ACCOUNT_KEY}"))
                && call.x_api_key.is_none()
        }),
        "{calls:?}"
    );

    let logs = harness.forward_logs().await;
    let item = logs["items"]
        .as_array()
        .and_then(|items| items.first())
        .unwrap();
    assert_eq!(item["providerId"].as_str(), Some(CUSTOM_PROVIDER_ID));
    assert_eq!(item["accountId"], account["id"]);
    assert_eq!(item["requestedModel"].as_str(), Some(CUSTOM_MODEL));
    assert_eq!(item["upstreamModel"].as_str(), Some(CUSTOM_MODEL));
    assert_eq!(item["costState"].as_str(), Some("unknown"));
    assert!(item["cost"].is_null(), "{item}");
    harness.shutdown();
}

#[tokio::test]
async fn public_custom_alias_materializes_upstream_model_and_logs_all_three_identities() {
    let harness = BlackBoxHarness::start_with_upstream(Some(protocol_success_replies(
        &[CUSTOM_ACCOUNT_KEY],
        SUCCESS_CHAT_BODY,
    )))
    .await;
    let account = create_verified_enabled_custom_mapping(
        &harness,
        "mapped-custom",
        CUSTOM_ACCOUNT_KEY,
        MAPPED_PUBLIC_MODEL,
        MAPPED_UPSTREAM_MODEL,
    )
    .await;

    let catalog = harness.list_client_models().await.1;
    let ids = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&MAPPED_PUBLIC_MODEL), "{catalog}");
    assert!(!ids.contains(&MAPPED_UPSTREAM_MODEL), "{catalog}");

    let verification_calls = harness.fake_calls();
    assert!(verification_calls.iter().any(|call| {
        serde_json::from_str::<Value>(&call.body)
            .is_ok_and(|body| body["model"] == MAPPED_UPSTREAM_MODEL)
    }));
    let calls_before = verification_calls.len();
    let (status, body) = harness.chat(MAPPED_PUBLIC_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = harness.fake_calls();
    assert_eq!(calls.len(), calls_before + 1);
    let upstream_body: Value = serde_json::from_str(&calls.last().unwrap().body).unwrap();
    assert_eq!(upstream_body["model"], MAPPED_UPSTREAM_MODEL);

    let logs = harness.forward_logs().await;
    let item = logs["items"]
        .as_array()
        .and_then(|items| items.first())
        .unwrap();
    assert_eq!(item["accountId"], account["id"]);
    assert_eq!(item["requestedModel"], MAPPED_PUBLIC_MODEL);
    assert_eq!(item["resolvedAlias"], MAPPED_PUBLIC_MODEL);
    assert_eq!(item["upstreamModel"], MAPPED_UPSTREAM_MODEL);
    harness.shutdown();
}

#[tokio::test]
async fn responses_and_messages_use_configured_protocol_and_auth_isolation() {
    let mut replies = protocol_success_replies(&[CUSTOM_ACCOUNT_KEY], SUCCESS_RESPONSES_BODY);
    replies.insert(
        CUSTOM_KEY_2.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: SUCCESS_MESSAGES_BODY,
        }]),
    );
    let harness = BlackBoxHarness::start_with_upstream(Some(replies)).await;

    let responses = create_verified_enabled_custom(
        &harness,
        "custom-responses",
        CUSTOM_ACCOUNT_KEY,
        "custom-responses-model",
        "responses",
        "bearer",
    )
    .await;
    let messages = create_verified_enabled_custom(
        &harness,
        "custom-messages",
        CUSTOM_KEY_2,
        "custom-messages-model",
        "messages",
        "x-api-key",
    )
    .await;

    let response = harness
        .client
        .post(harness.gateway("/v1/responses"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": "custom-responses-model",
            "input": "ping",
            "max_output_tokens": 3,
            "store": false,
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = decode_json_value(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"].as_str(), Some("custom-responses-model"));

    let response = harness
        .client
        .post(harness.gateway("/v1/messages"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": "custom-messages-model",
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = decode_json_value(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"].as_str(), Some("custom-messages-model"));

    let calls = harness.fake_calls();
    assert!(
        calls.iter().any(|call| {
            call.path.ends_with("/responses")
                && call.key == CUSTOM_ACCOUNT_KEY
                && call.authorization.is_some()
                && call.x_api_key.is_none()
        }),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call.path.ends_with("/messages")
                && call.key == CUSTOM_KEY_2
                && call.x_api_key.as_deref() == Some(CUSTOM_KEY_2)
                && call.authorization.is_none()
        }),
        "{calls:?}"
    );
    let _ = (responses, messages);
    harness.shutdown();
}

async fn decode_json_value(response: reqwest::Response) -> Value {
    let text = response.text().await.unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }))
}

#[tokio::test]
async fn overlap_keeps_go_mapping_and_undeclared_models_are_excluded() {
    let mut replies =
        protocol_success_replies(&[GO_ACCOUNT_KEY, CUSTOM_ACCOUNT_KEY], SUCCESS_CHAT_BODY);
    replies.insert(
        CUSTOM_KEY_2.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: SUCCESS_CHAT_BODY,
        }]),
    );
    let harness = BlackBoxHarness::start_with_upstream(Some(replies)).await;
    let go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    let custom = create_verified_enabled_custom(
        &harness,
        "custom-overlap",
        CUSTOM_ACCOUNT_KEY,
        GO_ALIAS,
        "chat_completions",
        "bearer",
    )
    .await;
    let other = create_verified_enabled_custom(
        &harness,
        "custom-other",
        CUSTOM_KEY_2,
        CUSTOM_MODEL_2,
        "chat_completions",
        "bearer",
    )
    .await;

    // The stripped Zen alias intentionally shares `deepseek-v4-flash` with
    // Go. Put Go first so this overlap test proves that Custom cannot steal a
    // published alias without contradicting the account-order contract.
    let mut account_ids = harness
        .accounts()
        .await
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|account| account["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|id| {
        if id == go["id"].as_str().unwrap() {
            0
        } else {
            1
        }
    });
    let (status, body) = harness
        .put_json("/accounts/order", &json!({ "accountIds": account_ids }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = custom;

    let (status, body) = harness.chat(GO_ALIAS).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"].as_str(), Some(GO_ALIAS));
    let logs = harness.forward_logs().await;
    let item = logs["items"]
        .as_array()
        .and_then(|items| items.first())
        .unwrap();
    assert_eq!(item["accountId"], go["id"]);
    assert_eq!(item["providerId"].as_str(), Some(OPENCODE_PROVIDER_ID));

    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "undeclared Custom model must not route: {body}"
    );
    let (status, body) = harness.chat(CUSTOM_MODEL_2).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let logs = harness.forward_logs().await;
    let items = logs["items"].as_array().cloned().unwrap_or_default();
    assert!(
        items.iter().any(
            |item| item["accountId"] == other["id"] && item["requestedModel"] == CUSTOM_MODEL_2
        ),
        "{items:?}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn same_custom_model_uses_account_order_and_config_change_stales() {
    let mut replies = protocol_success_replies(&[CUSTOM_ACCOUNT_KEY], SUCCESS_CHAT_BODY);
    replies.insert(
        CUSTOM_KEY_2.to_string(),
        VecDeque::from([
            FakeReply {
                status: 200,
                body: SUCCESS_CHAT_BODY,
            },
            FakeReply {
                status: 401,
                body: r#"{"error":"unauthorized"}"#,
            },
            FakeReply {
                status: 200,
                body: SUCCESS_CHAT_BODY,
            },
        ]),
    );
    let harness = BlackBoxHarness::start_with_upstream(Some(replies)).await;
    let first = create_verified_enabled_custom(
        &harness,
        "custom-a",
        CUSTOM_KEY_2,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
    )
    .await;
    let second = create_verified_enabled_custom(
        &harness,
        "custom-b",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
    )
    .await;

    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keys = harness.fake_call_keys();
    assert!(
        keys.iter().any(|key| key == CUSTOM_KEY_2)
            && keys.iter().any(|key| key == CUSTOM_ACCOUNT_KEY),
        "same model must try ordered Custom accounts: {keys:?}"
    );
    let logs = harness.forward_logs().await;
    let items = logs["items"].as_array().cloned().unwrap_or_default();
    assert!(items.iter().any(|item| item["accountId"] == second["id"]));

    let (status, updated) = harness
        .put_json(
            &format!("/accounts/{}/custom-config", first["id"].as_str().unwrap()),
            &json!({
                "endpointUrl": format!("{}/v2/chat/completions", custom_origin(&harness)),
                "upstreamProtocol": "chat_completions",
                "modelCapabilities": [{
                    "modelId": CUSTOM_MODEL,
                    "protocol": "chat_completions"
                }]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let updated = mutation_account(updated);
    assert_eq!(
        updated["enabled"], true,
        "config edits keep the account enabled: {updated}"
    );
    assert_eq!(
        updated["verificationStatus"].as_str(),
        Some("pending"),
        "{updated}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn custom_stream_does_not_cross_account_retry_after_output() {
    let harness = start_with_disconnect_upstream().await;
    let origin = custom_origin(&harness);
    let first = {
        let (status, draft) = harness
            .create_account(json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "custom-one",
                "key": CUSTOM_ACCOUNT_KEY,
                "expectedRevision": harness.settings_revision().await,
                "customConfig": {
                    "endpointUrl": custom_endpoint(&origin, "chat_completions"),
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "modelId": CUSTOM_MODEL,
                    "protocol": "chat_completions"
                }]
            }))
            .await;
        assert_eq!(status, StatusCode::OK, "{draft}");
        draft
    };
    harness
        .state
        .db
        .lock()
        .set_account_verification(
            first["id"].as_str().unwrap(),
            ocg_core::provider::ConnectionVerificationStatus::Verified,
            Some(chrono::Utc::now()),
            None,
        )
        .unwrap();
    let id = first["id"].as_str().unwrap().to_string();
    ensure_account_enabled(&harness, &id).await;

    let second_id = {
        let (status, draft) = harness
            .create_account(json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "custom-two",
                "key": CUSTOM_KEY_2,
                "expectedRevision": harness.settings_revision().await,
                "customConfig": {
                    "endpointUrl": custom_endpoint(&origin, "chat_completions"),
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "modelId": CUSTOM_MODEL,
                    "protocol": "chat_completions"
                }]
            }))
            .await;
        assert_eq!(status, StatusCode::OK, "{draft}");
        let id = draft["id"].as_str().unwrap().to_string();
        harness
            .state
            .db
            .lock()
            .set_account_verification(
                &id,
                ocg_core::provider::ConnectionVerificationStatus::Verified,
                Some(chrono::Utc::now()),
                None,
            )
            .unwrap();
        ensure_account_enabled(&harness, &id).await;
        id
    };
    let _ = second_id;

    let response = harness
        .client
        .post(harness.gateway("/v1/chat/completions"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CUSTOM_MODEL,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("ok") || body.contains("delta"),
        "client must have seen output before the disconnect: {body}"
    );
    assert_eq!(
        harness.disconnect_call_count(),
        1,
        "output already started; Custom must not retry another account"
    );
    harness.shutdown();
}

#[tokio::test]
async fn goat_raw_overlap_with_custom_is_ambiguous_and_does_not_call_upstream() {
    let harness = BlackBoxHarness::start_with_upstream(Some(protocol_success_replies(
        &[CUSTOM_ACCOUNT_KEY],
        SUCCESS_CHAT_BODY,
    )))
    .await;
    let _custom = create_verified_enabled_custom(
        &harness,
        "custom-goat-raw",
        CUSTOM_ACCOUNT_KEY,
        GOAT_UNIQUE_RAW_ID,
        "chat_completions",
        "bearer",
    )
    .await;
    let catalog = harness.list_client_models().await.1;
    assert!(
        !catalog["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == GOAT_UNIQUE_RAW_ID),
        "ambiguous raw/public collision must not be published: {catalog}"
    );
    let calls_before = harness.fake_calls().len();
    let (status, body) = harness.chat(GOAT_UNIQUE_RAW_ID).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(error_type(&body), Some(AMBIGUOUS_ERROR_TYPE), "{body}");
    assert_eq!(
        harness.fake_calls().len(),
        calls_before,
        "ambiguous raw id must not call upstream: {:?}",
        harness.fake_calls()
    );
    harness.shutdown();
}

async fn create_pending_custom(
    harness: &BlackBoxHarness,
    name: &str,
    key: &str,
    model_id: &str,
    protocol: &str,
    _auth_scheme: &str,
    base_url: &str,
) -> Value {
    let endpoint = custom_endpoint(base_url, protocol);
    let (status, draft) = harness
        .create_account(json!({
            "providerId": CUSTOM_PROVIDER_ID,
            "name": name,
            "key": key,
            "expectedRevision": harness.settings_revision().await,
            "customConfig": {
                "endpointUrl": endpoint,
                "upstreamProtocol": protocol
            },
            "modelCapabilities": [{
                "modelId": model_id,
                "protocol": protocol
            }]
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert_eq!(draft["verificationStatus"].as_str(), Some("pending"));
    draft
}

struct HeldJsonServer {
    base_url: String,
    hits: Arc<AtomicUsize>,
    release: tokio::sync::watch::Sender<bool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HeldJsonServer {
    async fn start(status: u16, body: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("hold listener");
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let (release, release_rx) = tokio::sync::watch::channel(false);
        let (stop, mut shutdown) = tokio::sync::oneshot::channel();
        let body = body.to_string();
        let hits_task = hits.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { break };
                        let hits = hits_task.clone();
                        let body = body.clone();
                        let mut release_rx = release_rx.clone();
                        tokio::spawn(async move {
                            let mut buf = vec![0_u8; 8192];
                            let _ = stream.read(&mut buf).await;
                            hits.fetch_add(1, Ordering::SeqCst);
                            while !*release_rx.borrow() {
                                if release_rx.changed().await.is_err() {
                                    return;
                                }
                            }
                            let response = format!(
                                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            hits,
            release,
            stop: Some(stop),
        }
    }

    async fn wait_hits(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while self.hits.load(Ordering::SeqCst) < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {count} held probes, have {}",
                self.hits.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn release(&self) {
        let _ = self.release.send(true);
    }
}

impl Drop for HeldJsonServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn serve_once_after_delay(
    delay: Duration,
    status: u16,
    content_type: &str,
    body: &str,
) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delayed listener");
    let addr = listener.local_addr().unwrap();
    let body = body.to_string();
    let content_type = content_type.to_string();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 8192];
        let _ = stream.read(&mut buf).await;
        tokio::time::sleep(delay).await;
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}")
}

async fn serve_sse_after_delay(body_delay: Duration, payload: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("sse listener");
    let addr = listener.local_addr().unwrap();
    let payload = payload.to_string();
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let mut buf = vec![0_u8; 8192];
        let _ = stream.read(&mut buf).await;
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(headers.as_bytes()).await;
        let _ = stream.flush().await;
        tokio::time::sleep(body_delay).await;
        let _ = stream.write_all(payload.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    format!("http://{addr}")
}

async fn mark_verified_and_enable(harness: &BlackBoxHarness, id: &str) {
    harness
        .state
        .db
        .lock()
        .set_account_verification(
            id,
            ocg_core::provider::ConnectionVerificationStatus::Verified,
            Some(chrono::Utc::now()),
            None,
        )
        .unwrap();
    ensure_account_enabled(harness, id).await;
}

async fn patch_account_key(harness: &BlackBoxHarness, id: &str, key: &str) -> (StatusCode, Value) {
    let (status, body) = harness
        .patch_json(
            &format!("/accounts/{id}"),
            &json!({
                "key": key,
                "expectedRevision": harness.settings_revision().await
            }),
        )
        .await;
    if status.is_success() {
        (status, mutation_account(body))
    } else {
        (status, body)
    }
}

#[tokio::test]
async fn delayed_verify_probe_conflicts_on_key_config_caps_delete_and_concurrent() {
    async fn delayed_key_race() {
        let held = HeldJsonServer::start(200, SUCCESS_CHAT_BODY).await;
        let harness = BlackBoxHarness::start().await;
        let draft = create_pending_custom(
            &harness,
            "cas-key",
            CUSTOM_ACCOUNT_KEY,
            CUSTOM_MODEL,
            "chat_completions",
            "bearer",
            &held.base_url,
        )
        .await;
        let id = draft["id"].as_str().unwrap().to_string();
        let verify = tokio::spawn({
            let client = harness.client.clone();
            let url = harness.dashboard(&format!("/accounts/{id}/verify"));
            let body = harness.mutation_body(json!({}));
            async move { client.post(url).json(&body).send().await.unwrap() }
        });
        held.wait_hits(1).await;
        let (status, updated) = patch_account_key(&harness, &id, CUSTOM_KEY_2).await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["verificationStatus"].as_str(), Some("pending"));
        held.release();
        let response = verify.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let after = harness.account_by_id(&id).await;
        assert_eq!(after["verificationStatus"].as_str(), Some("pending"));
        harness.shutdown();
    }

    async fn delayed_config_race() {
        let held = HeldJsonServer::start(200, SUCCESS_CHAT_BODY).await;
        let harness = BlackBoxHarness::start().await;
        let draft = create_pending_custom(
            &harness,
            "cas-config",
            CUSTOM_ACCOUNT_KEY,
            CUSTOM_MODEL,
            "chat_completions",
            "bearer",
            &held.base_url,
        )
        .await;
        let id = draft["id"].as_str().unwrap().to_string();
        let verify = tokio::spawn({
            let client = harness.client.clone();
            let url = harness.dashboard(&format!("/accounts/{id}/verify"));
            let body = harness.mutation_body(json!({}));
            async move { client.post(url).json(&body).send().await.unwrap() }
        });
        held.wait_hits(1).await;
        let (status, _updated) = harness
            .put_json(
                &format!("/accounts/{id}/custom-config"),
                &json!({
                    "endpointUrl": "http://127.0.0.1:1/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "modelCapabilities": [{
                        "modelId": CUSTOM_MODEL,
                        "protocol": "chat_completions"
                    }]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        held.release();
        let response = verify.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let after = harness.account_by_id(&id).await;
        assert_eq!(after["verificationStatus"].as_str(), Some("pending"));
        assert_eq!(
            after["customConfig"]["endpointUrl"].as_str(),
            Some("http://127.0.0.1:1/v1/chat/completions")
        );
        harness.shutdown();
    }

    async fn delayed_capability_race() {
        let held = HeldJsonServer::start(200, SUCCESS_CHAT_BODY).await;
        let harness = BlackBoxHarness::start().await;
        let draft = create_pending_custom(
            &harness,
            "cas-caps",
            CUSTOM_ACCOUNT_KEY,
            CUSTOM_MODEL,
            "chat_completions",
            "bearer",
            &held.base_url,
        )
        .await;
        let id = draft["id"].as_str().unwrap().to_string();
        let verify = tokio::spawn({
            let client = harness.client.clone();
            let url = harness.dashboard(&format!("/accounts/{id}/verify"));
            let body = harness.mutation_body(json!({}));
            async move { client.post(url).json(&body).send().await.unwrap() }
        });
        held.wait_hits(1).await;
        let (status, _updated) = harness
            .put_json(
                &format!("/accounts/{id}/model-capabilities"),
                &json!({
                    "capabilities": [{
                        "publicModel": CUSTOM_MODEL,
                        "upstreamModel": CUSTOM_MODEL_2,
                        "protocol": "chat_completions"
                    }]
                }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        held.release();
        let response = verify.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let after = harness.account_by_id(&id).await;
        assert_eq!(after["verificationStatus"].as_str(), Some("pending"));
        assert_eq!(after["modelCapabilities"][0]["publicModel"], CUSTOM_MODEL);
        assert_eq!(
            after["modelCapabilities"][0]["upstreamModel"],
            CUSTOM_MODEL_2
        );
        harness.shutdown();
    }

    async fn delayed_delete_race() {
        let held = HeldJsonServer::start(200, SUCCESS_CHAT_BODY).await;
        let harness = BlackBoxHarness::start().await;
        let draft = create_pending_custom(
            &harness,
            "cas-delete",
            CUSTOM_ACCOUNT_KEY,
            CUSTOM_MODEL,
            "chat_completions",
            "bearer",
            &held.base_url,
        )
        .await;
        let id = draft["id"].as_str().unwrap().to_string();
        let verify = tokio::spawn({
            let client = harness.client.clone();
            let url = harness.dashboard(&format!("/accounts/{id}/verify"));
            let body = harness.mutation_body(json!({}));
            async move { client.post(url).json(&body).send().await.unwrap() }
        });
        held.wait_hits(1).await;
        let (status, _deleted) = harness
            .delete_json(&format!("/accounts/{id}"), &json!({}))
            .await;
        assert_eq!(status, StatusCode::OK);
        held.release();
        let response = verify.await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let accounts = harness.accounts().await;
        assert!(
            accounts
                .as_array()
                .into_iter()
                .flatten()
                .all(|account| account["id"] != id)
        );
        harness.shutdown();
    }

    async fn delayed_concurrent_verifies() {
        let held = HeldJsonServer::start(200, SUCCESS_CHAT_BODY).await;
        let harness = BlackBoxHarness::start().await;
        let draft = create_pending_custom(
            &harness,
            "cas-concurrent",
            CUSTOM_ACCOUNT_KEY,
            CUSTOM_MODEL,
            "chat_completions",
            "bearer",
            &held.base_url,
        )
        .await;
        let id = draft["id"].as_str().unwrap().to_string();
        let spawn_verify = || {
            let client = harness.client.clone();
            let url = harness.dashboard(&format!("/accounts/{id}/verify"));
            let body = harness.mutation_body(json!({}));
            async move { client.post(url).json(&body).send().await.unwrap() }
        };
        let first = tokio::spawn(spawn_verify());
        let second = tokio::spawn(spawn_verify());
        held.wait_hits(2).await;
        held.release();
        let first = first.await.unwrap();
        let second = second.await.unwrap();
        let statuses = [first.status(), second.status()];
        assert!(
            statuses.contains(&StatusCode::OK) && statuses.contains(&StatusCode::CONFLICT),
            "concurrent verifies must certify once and 409 the other: {statuses:?}"
        );
        let after = harness.account_by_id(&id).await;
        assert_eq!(after["verificationStatus"].as_str(), Some("verified"));
        harness.shutdown();
    }

    delayed_key_race().await;
    delayed_config_race().await;
    delayed_capability_race().await;
    delayed_delete_race().await;
    delayed_concurrent_verifies().await;
}

#[tokio::test]
async fn custom_overlay_of_chat_preferred_builtin_preserves_native_structured_formats() {
    let mut replies = protocol_success_replies(&[CUSTOM_ACCOUNT_KEY], SUCCESS_RESPONSES_BODY);
    replies.insert(
        CUSTOM_KEY_2.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: SUCCESS_MESSAGES_BODY,
        }]),
    );
    let harness = BlackBoxHarness::start_with_upstream(Some(replies)).await;
    let _responses = create_verified_enabled_custom(
        &harness,
        "custom-responses-overlay",
        CUSTOM_ACCOUNT_KEY,
        CHAT_PREFERRED_BUILTIN_ALIAS,
        "responses",
        "bearer",
    )
    .await;
    let _messages = create_verified_enabled_custom(
        &harness,
        "custom-messages-overlay",
        CUSTOM_KEY_2,
        CHAT_PREFERRED_BUILTIN_ALIAS,
        "messages",
        "x-api-key",
    )
    .await;
    let response = harness
        .client
        .post(harness.gateway("/v1/responses"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CHAT_PREFERRED_BUILTIN_ALIAS,
            "input": "ping",
            "max_output_tokens": 3,
            "store": false,
            "stream": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": {"type": "object"}
                }
            }
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = decode_json_value(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = harness
        .client
        .post(harness.gateway("/v1/messages"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CHAT_PREFERRED_BUILTIN_ALIAS,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false,
            "output_config": {
                "format": {"type": "json_schema", "schema": {"type": "object"}}
            }
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = decode_json_value(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let calls = harness.fake_calls();
    assert!(
        calls.iter().any(|call| {
            call.path.ends_with("/responses")
                && call.key == CUSTOM_ACCOUNT_KEY
                && call.body.contains("json_schema")
                && call.body.contains("text")
        }),
        "native Responses text.format must be forwarded: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call.path.ends_with("/messages")
                && call.key == CUSTOM_KEY_2
                && call.body.contains("output_config")
                && call.body.contains("json_schema")
        }),
        "native Messages output_config.format must be forwarded: {calls:?}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn pure_builtin_chat_preferred_alias_rejects_structured_conversion_without_upstream() {
    let harness = BlackBoxHarness::start_with_upstream(Some(protocol_success_replies(
        &[GO_ACCOUNT_KEY],
        SUCCESS_CHAT_BODY,
    )))
    .await;
    let _go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;

    let response = harness
        .client
        .post(harness.gateway("/v1/responses"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CHAT_PREFERRED_BUILTIN_ALIAS,
            "input": "ping",
            "store": false,
            "stream": false,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "answer",
                    "schema": {"type": "object"}
                }
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = harness
        .client
        .post(harness.gateway("/v1/messages"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CHAT_PREFERRED_BUILTIN_ALIAS,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false,
            "output_config": {
                "format": {"type": "json_schema", "schema": {"type": "object"}}
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        harness.fake_calls().is_empty(),
        "incompatible pure-builtin requests must fail before any upstream call: {:?}",
        harness.fake_calls()
    );
    harness.shutdown();
}

#[tokio::test]
async fn custom_timeouts_use_connect_and_per_request_limits() {
    let delayed = serve_once_after_delay(
        Duration::from_millis(1500),
        200,
        "application/json",
        SUCCESS_CHAT_BODY,
    )
    .await;
    let harness = BlackBoxHarness::start().await;
    let draft = create_pending_custom(
        &harness,
        "custom-timeout",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
        &delayed,
    )
    .await;
    let id = draft["id"].as_str().unwrap().to_string();
    mark_verified_and_enable(&harness, &id).await;
    let mut config = harness.state.config();
    config.connect_timeout_secs = 1;
    config.non_stream_timeout_secs = 4;
    config.stream_idle_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "non-stream Custom may outlive connect_timeout: {body}"
    );
    harness.shutdown();

    let too_slow = serve_once_after_delay(
        Duration::from_secs(3),
        200,
        "application/json",
        SUCCESS_CHAT_BODY,
    )
    .await;
    let harness = BlackBoxHarness::start().await;
    let draft = create_pending_custom(
        &harness,
        "custom-timeout-fail",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
        &too_slow,
    )
    .await;
    let id = draft["id"].as_str().unwrap().to_string();
    mark_verified_and_enable(&harness, &id).await;
    let mut config = harness.state.config();
    config.connect_timeout_secs = 5;
    config.non_stream_timeout_secs = 1;
    config.stream_idle_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "non-stream must honor timeout: {body}"
    );
    harness.shutdown();

    let sse = serve_sse_after_delay(
        Duration::from_millis(1500),
        concat!(
            "data: {\"id\":\"chat-stream\",\"model\":\"custom-local-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        ),
    )
    .await;
    let harness = BlackBoxHarness::start().await;
    let draft = create_pending_custom(
        &harness,
        "custom-stream-timeout",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
        &sse,
    )
    .await;
    let id = draft["id"].as_str().unwrap().to_string();
    mark_verified_and_enable(&harness, &id).await;
    let mut config = harness.state.config();
    config.connect_timeout_secs = 1;
    config.non_stream_timeout_secs = 1;
    config.stream_idle_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
    let response = harness
        .client
        .post(harness.gateway("/v1/chat/completions"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": CUSTOM_MODEL,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(
        body.contains("ok") || body.contains("delta"),
        "streaming Custom may outlive non_stream_timeout: {body}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn oversized_verification_body_fails_cleanly() {
    let pad = "x".repeat(MAX_CUSTOM_VERIFICATION_BODY_BYTES);
    let huge = format!(r#"{{"id":"ok","pad":"{pad}"}}"#);
    let origin =
        serve_once_after_delay(Duration::from_millis(0), 200, "application/json", &huge).await;
    let harness = BlackBoxHarness::start().await;
    let draft = create_pending_custom(
        &harness,
        "custom-oversize",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
        &origin,
    )
    .await;
    let id = draft["id"].as_str().unwrap().to_string();
    let (status, body) = verify_account(&harness, &id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["enabled"], true,
        "failed verify must not change the card's enabled state: {body}"
    );
    assert_eq!(body["verificationStatus"].as_str(), Some("failed"));
    assert!(
        body["verificationError"]
            .as_str()
            .is_some_and(|error| error.contains("exceeded")),
        "{body}"
    );
    harness.shutdown();
}

#[tokio::test]
async fn unknown_custom_429_uses_temporary_wait_without_inventing_quota_windows() {
    let harness = BlackBoxHarness::start_with_upstream(Some({
        let mut replies = HashMap::new();
        replies.insert(
            CUSTOM_ACCOUNT_KEY.to_string(),
            VecDeque::from([
                FakeReply {
                    status: 200,
                    body: SUCCESS_CHAT_BODY,
                },
                FakeReply {
                    status: 429,
                    body: "5-hour usage limit reached. Resets in 13min.",
                },
            ]),
        );
        replies
    }))
    .await;
    let account = create_verified_enabled_custom(
        &harness,
        "custom-429",
        CUSTOM_ACCOUNT_KEY,
        CUSTOM_MODEL,
        "chat_completions",
        "bearer",
    )
    .await;
    let id = account["id"].as_str().unwrap().to_string();
    let (status, body) = harness.chat(CUSTOM_MODEL).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    let after = harness.account_by_id(&id).await;
    assert!(
        after["cooldownGenericUntil"].is_null() && after["cooldownUntil"].is_null(),
        "an unknown Custom 429 must not invent account cooldown: {after}"
    );
    assert!(
        after["cooldown5hUntil"].is_null(),
        "Custom 429 must not parse Go 5-hour windows: {after}"
    );
    let (again_status, again_body) = harness.chat(CUSTOM_MODEL).await;
    assert_eq!(
        again_status,
        StatusCode::TOO_MANY_REQUESTS,
        "a temporary 429 wait must block an immediate retry: {again_body}"
    );
    assert_eq!(again_body["error"]["type"], "rate_limit_error");
    assert_eq!(
        harness.fake_calls().len(),
        2,
        "one verification and one inference call; the retry stays local"
    );
    assert!(after["authError"].is_null());
    assert!(after["lastError"].is_null());
    harness.shutdown();
}
