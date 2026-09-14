//! Black-box Alias and multi-Plan contract tests.
//!
//! These tests drive public Gateway and dashboard HTTP/JSON. They are the
//! independent acceptance slice for the accepted unified-alias / multi-Plan
//! contracts. Command Code refreshes its public catalog with GET `/models`. Custom is catalog-routable with
//! an available verification runtime; live Custom network coverage lives in
//! `custom_trusted_admin.rs`.
//!
//! Out of scope: live GOAT network calls.

use reqwest::StatusCode;
use serde_json::json;

#[path = "fixtures/blackbox/harness.rs"]
mod harness;

use harness::*;

async fn reorder_account_first(harness: &BlackBoxHarness, account_id: &str) {
    let mut account_ids = harness
        .accounts()
        .await
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|account| account["id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    account_ids.sort_by_key(|id| if id == account_id { 0 } else { 1 });
    let (status, body) = harness
        .put_json("/accounts/order", &json!({ "accountIds": account_ids }))
        .await;
    assert_eq!(status, StatusCode::OK, "account reorder failed: {body}");
}

/// Unknown offerings fail closed at the dashboard create gate.
#[tokio::test]
async fn unknown_offering_create_fails_closed() {
    let harness = BlackBoxHarness::start().await;
    let before = harness.accounts().await;
    let (status, body) = harness
        .create_account(json!({
            "providerId": "not-a-provider",
            "name": "should-not-exist",
            "key": GO_ACCOUNT_KEY,
            "expectedRevision": harness.settings_revision().await
        }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        !json_contains_secret(&body, GO_ACCOUNT_KEY),
        "unknown-offering error leaked the Key: {body}"
    );
    let after = harness.accounts().await;
    assert_eq!(
        after.as_array().map(Vec::len),
        before.as_array().map(Vec::len),
        "unknown offering must not persist an account: {after}"
    );
    harness.shutdown();
}

/// A unique raw upstream ID is pinned to one provider. With only Go
/// routeable, the GOAT-shaped raw id must not fall through to OpenCode Go.
#[tokio::test]
async fn unique_raw_upstream_id_pins_to_one_provider_and_skips_go() {
    let harness = BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY]).await;
    let go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    let (status, body) = harness.chat(GOAT_UNIQUE_RAW_ID).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "raw id {GOAT_UNIQUE_RAW_ID} is uniquely GOAT and must not succeed on Go: {body}"
    );
    assert_eq!(
        harness.fake_call_keys(),
        Vec::<String>::new(),
        "unique raw id must pin to command-code/goat and must not call the Go upstream"
    );
    let logs = harness.forward_logs().await;
    for item in logs["items"].as_array().unwrap_or(&Vec::new()) {
        assert_ne!(
            item["providerId"].as_str(),
            Some(OPENCODE_PROVIDER_ID),
            "raw GOAT id was attributed to Go: {item}"
        );
        assert_ne!(
            item["accountId"], go["id"],
            "raw GOAT id was routed to the Go account: {item}"
        );
    }
    harness.shutdown();
}

/// An unmapped raw upstream ID fails closed and never reaches an upstream.
///
/// Structured `ambiguous_model_id` coverage lives in
/// `v2_alias_runtime::ambiguous_model_id_is_structured_across_client_formats`.
#[tokio::test]
async fn unmapped_raw_upstream_id_is_rejected() {
    let harness =
        BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY, CUSTOM_ACCOUNT_KEY]).await;
    let _go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    let catalog = harness.catalog().await;
    let custom =
        catalog_entry(&catalog, CUSTOM_PROVIDER_ID).expect("catalog must include custom/api");
    assert_eq!(
        custom["routable"], true,
        "custom/api is catalog-routable: {custom}"
    );
    assert_eq!(
        custom["verificationRuntimeAvailability"].as_str(),
        Some("available"),
        "custom/api verification runtime is available: {custom}"
    );

    let (status, body) = harness.chat(CUSTOM_OVERLAP_RAW_ID).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "unmapped raw id {CUSTOM_OVERLAP_RAW_ID} must fail closed rather than fall through to Go: {body}"
    );
    assert_ne!(
        error_type(&body),
        Some(AMBIGUOUS_ERROR_TYPE),
        "live registry has no overlapping raw ids; {CUSTOM_OVERLAP_RAW_ID} is not an invented Custom ambiguous route: {body}"
    );
    assert!(
        harness
            .fake_call_keys()
            .into_iter()
            .all(|key| key != GO_ACCOUNT_KEY && key != CUSTOM_ACCOUNT_KEY),
        "unmapped raw ids must not call any upstream: {:?}",
        harness.fake_calls()
    );
    harness.shutdown();
}

/// Zen Free stays anonymous and does not send an account Key.
#[tokio::test]
async fn zen_free_explicit_free_model_stays_anonymous() {
    let harness = BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY]).await;
    let _go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    harness
        .state
        .activate_zen_free_model_catalog(ocg_core::kernel::zen::ZenFreeModelCatalog {
            models: vec![FREE_MODEL.to_string()],
            refreshed_at: Some(chrono::Utc::now()),
            source_url: ocg_core::kernel::zen::ZEN_MODELS_SOURCE_URL.to_string(),
        })
        .unwrap();
    {
        let now = chrono::Utc::now();
        let scope = ocg_core::provider_contracts::ContractScope::provider(
            ocg_core::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
        );
        harness
            .state
            .db
            .lock()
            .set_model_protocol_overrides(
                &scope,
                &[(
                    FREE_MODEL.to_string(),
                    ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
                    ocg_core::provider_contracts::ProtocolOverrideState::ForceOn,
                )],
                now,
            )
            .unwrap();
        harness.state.reload_provider_contracts().unwrap();
    }
    let revision = harness.settings_revision().await;
    let (status, body) = harness
        .patch_json(
            "/providers/zen-free",
            &json!({
                "enabled": true,
                "expectedRevision": revision
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = harness.chat(FREE_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let keys = harness.fake_call_keys();
    assert_eq!(
        keys,
        vec![String::new()],
        "Zen Free must remain anonymous and must not rotate a Go Key: {keys:?}"
    );
    let calls = harness.fake_calls();
    assert!(
        calls.iter().all(|call| {
            call.authorization.is_none()
                && call.x_api_key.is_none()
                && call.x_goog_api_key.is_none()
        }),
        "Zen Free leaked an auth header: {calls:?}"
    );
    harness.shutdown();
}

/// Go import stays immediately routable; verification is not required.
#[tokio::test]
async fn go_import_remains_immediately_routable_without_verification() {
    let harness = BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY]).await;
    let account = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    assert_eq!(account["enabled"], true, "{account}");
    assert_eq!(account["setupStep"], "ready", "{account}");
    let status = account["verificationStatus"]
        .as_str()
        .unwrap_or("not_required");
    assert_eq!(
        status, "not_required",
        "Go import must not require connection verification: {account}"
    );
    let (chat_status, body) = harness.chat(GO_ALIAS).await;
    assert_eq!(chat_status, StatusCode::OK, "{body}");
    harness.shutdown();
}

/// GOAT is live without directory verification; Custom remains an optional-verification draft.
#[tokio::test]
async fn goat_creates_live_while_custom_creates_a_pending_draft() {
    let harness = BlackBoxHarness::start().await;
    let catalog = harness.catalog().await;

    let goat = catalog_entry(&catalog, COMMAND_CODE_PROVIDER_ID)
        .expect("catalog must include command-code/goat");
    assert_eq!(
        goat["verificationPolicy"].as_str(),
        Some("not_required"),
        "Command Code's public catalog must not be presented as Key verification: {goat}"
    );
    assert_eq!(
        goat["verificationRuntimeAvailability"].as_str(),
        Some("not_applicable")
    );
    assert_eq!(
        goat["creationAvailability"].as_str(),
        Some("available"),
        "GOAT accounts must be creatable: {goat}"
    );
    let (status, body) = harness
        .create_account(json!({
            "providerId": COMMAND_CODE_PROVIDER_ID,
            "name": "goat-live",
            "key": GOAT_ACCOUNT_KEY,
            "expectedRevision": harness.settings_revision().await
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["enabled"], true,
        "new GOAT Key accounts start enabled: {body}"
    );
    assert_eq!(
        body["verificationStatus"].as_str(),
        Some("not_required"),
        "GOAT directory refresh is not account verification: {body}"
    );
    assert!(
        body.get("key").is_none(),
        "account JSON must not return a Key field: {body}"
    );
    let goat_id = body["id"].as_str().expect("account id").to_string();
    let stored = harness.account_by_id(&goat_id).await;
    assert_eq!(
        stored["enabled"], true,
        "GOAT create must persist the default enabled switch: {stored}"
    );
    assert_eq!(
        stored["verificationStatus"].as_str(),
        Some("not_required"),
        "GOAT account must not expose a pending Key-verification state: {stored}"
    );
    assert!(
        stored["connectionVerifiedAt"].is_null()
            || stored
                .get("connectionVerifiedAt")
                .is_none_or(|value| value.as_str().is_none_or(|stamp| stamp.is_empty())),
        "connection_verified_at must remain unset: {stored}"
    );
    assert!(stored.get("key").is_none(), "{stored}");

    let (status, body) = harness
        .create_account(custom_create_payload(
            "custom-draft",
            CUSTOM_ACCOUNT_KEY,
            harness.settings_revision().await,
            &harness.upstream_base_url,
            CUSTOM_UNROUTABLE_MODEL_ID,
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["enabled"], true,
        "Custom creates enabled while verification stays pending: {body}"
    );
    assert_eq!(
        body["verificationStatus"].as_str(),
        Some("pending"),
        "Custom draft verification_status: {body}"
    );
    assert_eq!(
        body["planRoutable"], true,
        "Custom is catalog-routable: {body}"
    );
    assert_eq!(
        body["customConfig"]["endpointUrl"]
            .as_str()
            .map(|value| value.trim_end_matches("/chat/completions")),
        Some(harness.upstream_base_url.trim_end_matches('/')),
        "Custom create must persist the complete custom_config.endpoint_url: {body}"
    );
    let capabilities = body["modelCapabilities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        capabilities.iter().any(|item| {
            item["publicModel"] == CUSTOM_UNROUTABLE_MODEL_ID
                && item["upstreamModel"] == CUSTOM_UNROUTABLE_MODEL_ID
        }),
        "Custom create must persist model_capabilities: {body}"
    );
    harness.shutdown();
}

/// Explicitly disabled GOAT accounts must not be selected when a shared alias is requested.
#[tokio::test]
async fn disabled_goat_is_not_selected_for_alias_routing() {
    let harness =
        BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY, GOAT_ACCOUNT_KEY]).await;
    let go = harness.create_go_account("go-main", GO_ACCOUNT_KEY).await;
    let (status, goat) = harness
        .create_account(json!({
            "providerId": COMMAND_CODE_PROVIDER_ID,
            "name": "goat-disabled",
            "key": GOAT_ACCOUNT_KEY,
            "expectedRevision": harness.settings_revision().await
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{goat}");
    let goat_id = goat["id"].as_str().unwrap();
    harness.enable_stored_account(goat_id);
    let goat = harness.account_by_id(goat_id).await;
    assert_eq!(goat["enabled"], true, "{goat}");
    let (status, goat) = harness
        .patch_json(
            &format!("/accounts/{}", goat["id"].as_str().unwrap()),
            &json!({ "enabled": false }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{goat}");
    let goat = mutation_account(goat);
    assert_eq!(goat["enabled"], false, "{goat}");
    reorder_account_first(&harness, go["id"].as_str().unwrap()).await;

    let (status, body) = harness.chat(GO_ALIAS).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.fake_call_keys(), vec![GO_ACCOUNT_KEY.to_string()]);
    let logs = harness.forward_logs().await;
    let item = &logs["items"]
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or_else(|| panic!("expected a forward log: {logs}"));
    assert_eq!(item["accountId"], go["id"]);
    assert_ne!(item["accountId"], goat["id"]);
    harness.shutdown();
}

/// Account Keys stay out of dashboard JSON, errors, and logs.
#[tokio::test]
async fn account_secrets_absent_from_json_errors_and_logs() {
    let harness = BlackBoxHarness::start_with_chat_success(&[GO_ACCOUNT_KEY]).await;
    let account = harness.create_go_account("go-secret", GO_ACCOUNT_KEY).await;
    assert!(account.get("key").is_none(), "{account}");
    assert!(account.get("password").is_none(), "{account}");
    assert!(!json_contains_secret(&account, GO_ACCOUNT_KEY));

    let listed = harness.accounts().await;
    assert!(!json_contains_secret(&listed, GO_ACCOUNT_KEY));

    let (status, unknown) = harness.chat("definitely-not-a-model-or-alias").await;
    assert_ne!(status, StatusCode::UNAUTHORIZED, "{unknown}");
    assert!(!json_contains_secret(&unknown, GO_ACCOUNT_KEY));

    let _ = harness.chat(GO_ALIAS).await;
    let logs = harness.forward_logs().await;
    let gateway_logs = harness.gateway_logs().await;
    assert!(!json_contains_secret(&logs, GO_ACCOUNT_KEY), "{logs}");
    assert!(
        !json_contains_secret(&gateway_logs, GO_ACCOUNT_KEY),
        "{gateway_logs}"
    );
    assert!(!json_contains_secret(&logs, GATEWAY_KEY), "{logs}");

    let (conn_status, connection) = harness.get_json("/connection").await;
    assert_eq!(conn_status, StatusCode::OK, "{connection}");
    assert!(
        !json_contains_secret(&connection, GO_ACCOUNT_KEY),
        "connection info must not include the account Key: {connection}"
    );
    harness.shutdown();
}

/// After the client has seen output, alias routing must not hop accounts.
#[tokio::test]
async fn alias_stream_does_not_cross_account_retry_after_output() {
    let harness = start_with_disconnect_upstream().await;
    let first = harness.create_go_account("go-one", GO_ACCOUNT_KEY).await;
    let _second = harness.create_go_account("go-two", GO_ACCOUNT_KEY_2).await;
    reorder_account_first(&harness, first["id"].as_str().unwrap()).await;

    let response = harness
        .client
        .post(harness.gateway("/v1/chat/completions"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&json!({
            "model": GO_ALIAS,
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
        "output already started; upstream must not be retried on another account"
    );

    let logs = harness.forward_logs().await;
    let items = logs["items"].as_array().cloned().unwrap_or_default();
    let account_ids: Vec<String> = items
        .iter()
        .filter_map(|item| item["accountId"].as_str().map(str::to_string))
        .collect();
    let unique: std::collections::HashSet<_> = account_ids.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        1,
        "output already started; must not retry on another account: {items:?}"
    );
    assert_eq!(
        account_ids.first().map(String::as_str),
        first["id"].as_str()
    );
    harness.shutdown();
}
