//! Dashboard V4 connection projection and onboarding commit.

use chrono::{Duration, Utc};
use ocg_core::models::{Account, AccountSetupStep, AccountType, AccountUpdate, ProxyMode};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    ZEN_FREE_ACCOUNT_ID, default_credential_kind, default_quota_scope,
};
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{
    anonymous_binding_id_for, credential_id_for_legacy_account,
    observer_credential_id_for_platform_account,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};

#[allow(dead_code)]
#[path = "fixtures/fake_upstream.rs"]
mod fake_upstream;
#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use fake_upstream::{FakeReply, start_fake_upstream};
use harness::{V3Harness, start_loopback, start_public};

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = patch.as_object().cloned().unwrap_or_default();
    body.insert(
        "expectedRevision".into(),
        json!(harness.state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        json!(harness.state.process_generation()),
    );
    Value::Object(body)
}

fn v4_base(harness: &V3Harness) -> String {
    harness
        .v3_base
        .replacen("/dashboard/api/v3", "/dashboard/api/v4", 1)
}

async fn send_v3(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let parsed = response.json().await.unwrap_or(Value::Null);
    (status, parsed)
}

async fn send_v4(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", v4_base(harness)))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let parsed = response.json().await.unwrap_or(Value::Null);
    (status, parsed)
}

fn create_body(name: &str, endpoint: &str, protocol: &str, auth: &str, key: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "endpointUrl": endpoint,
        "upstreamProtocol": protocol,
        "authKind": auth,
        "models": [{
            "publicModel": "lab-opus",
            "upstreamModel": "vendor/opus"
        }]
    });
    if let Some(key) = key {
        body["key"] = json!(key);
    }
    body
}

fn json_field_names(value: &Value) -> Vec<&str> {
    match value {
        Value::Object(map) => {
            let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
            names.extend(map.values().flat_map(json_field_names));
            names
        }
        Value::Array(items) => items.iter().flat_map(json_field_names).collect(),
        _ => Vec::new(),
    }
}

fn json_string_values(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items.iter().flat_map(json_string_values).collect(),
        Value::Object(map) => map.values().flat_map(json_string_values).collect(),
        _ => Vec::new(),
    }
}

fn assert_secret_free(body: &Value, secrets: &[&str]) {
    for name in json_field_names(body) {
        assert!(
            !matches!(
                name,
                "key"
                    | "password"
                    | "passwordCipher"
                    | "keyCipher"
                    | "gatewayKey"
                    | "gateway_key"
                    | "apiKey"
                    | "secret"
            ),
            "secret-bearing field `{name}` leaked: {body}"
        );
    }
    for secret in secrets {
        for value in json_string_values(body) {
            assert!(
                !value.contains(secret),
                "secret `{secret}` leaked in {body}"
            );
        }
    }
}

fn connections_of(body: &Value) -> &[Value] {
    body["connections"].as_array().expect("connections array")
}

fn find_legacy<'a>(body: &'a Value, kind: &str, id: &str) -> &'a Value {
    connections_of(body)
        .iter()
        .find(|connection| connection["legacy"]["kind"] == kind && connection["legacy"]["id"] == id)
        .unwrap_or_else(|| panic!("missing {kind}:{id} in {body}"))
}

#[tokio::test]
async fn templates_list_builtins_and_custom_http_without_secrets_or_instances() {
    let harness = start_loopback("v4-templates").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Instance Lab",
                "https://lab.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                Some("sk-must-not-leak"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let instance_id = created["provider"]["id"].as_str().unwrap().to_string();

    let (status, templates) = send_v4(&harness, Method::GET, "/templates", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{templates}");
    assert_secret_free(&templates, &["sk-must-not-leak"]);
    let ids: Vec<&str> = templates["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|template| template["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&OPENCODE_PROVIDER_ID), "{ids:?}");
    assert!(ids.contains(&CUSTOM_PROVIDER_ID), "{ids:?}");
    assert!(ids.contains(&"custom-http"), "{ids:?}");
    assert!(!ids.contains(&CPA_PROVIDER_ID), "{ids:?}");
    assert!(
        !ids.contains(&instance_id.as_str()),
        "user instance leaked into templates: {ids:?}"
    );
    let custom_http = templates["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|template| template["id"] == "custom-http")
        .unwrap();
    assert_eq!(custom_http["adapterKind"], "configurable_http");
    assert_eq!(custom_http["source"], "builtin");
    assert_eq!(custom_http["pricingMultiplierEditable"], false);
    assert_eq!(
        custom_http["editableFields"],
        json!([
            "name",
            "endpointUrl",
            "upstreamProtocol",
            "authKind",
            "models"
        ])
    );
    let sealed = templates["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|template| template["id"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(sealed["editableFields"], json!([]));
    assert_eq!(sealed["pricingMultiplierEditable"], true);
    let goat = templates["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|template| template["id"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(goat["pricingMultiplierEditable"], true);
    harness.stop();
}

#[tokio::test]
async fn connections_project_keyless_dynamic_provider_as_missing_credential_and_ineligible() {
    let harness = start_loopback("v4-keyless").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Keyless Lab",
                "https://keyless.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["provider"]["id"].as_str().unwrap();
    let (status, body) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let connection = find_legacy(&body, "dynamic_provider", id);
    assert_eq!(connection["authorization"], "missing");
    assert_eq!(connection["eligibility"]["state"], "ineligible");
    assert_eq!(connection["eligibility"]["reason"], "missing_credential");
    assert_eq!(connection["credentialCount"], 0);
    harness.stop();
}

#[tokio::test]
async fn connections_project_dynamic_provider_with_unverified_enabled_key_as_unknown_and_eligible()
{
    let harness = start_loopback("v4-unverified").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Keyed Lab",
                "https://keyed.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                Some("sk-unverified"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["provider"]["id"].as_str().unwrap();
    let (status, body) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &["sk-unverified"]);
    let connection = find_legacy(&body, "dynamic_provider", id);
    assert_eq!(connection["authorization"], "unknown");
    assert_eq!(connection["lifecycle"], "configured");
    assert_eq!(connection["eligibility"]["state"], "eligible");
    assert_eq!(connection["eligibility"]["reason"], "none");
    assert_eq!(connection["credentialCount"], 1);
    assert_eq!(connection["enabledCredentialCount"], 1);

    let account_id = first_account_id_for(&harness, id);
    harness
        .state
        .db
        .lock()
        .update_account(
            &account_id,
            &AccountUpdate {
                enabled: Some(false),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
    let (status, disabled) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    let connection = find_legacy(&disabled, "dynamic_provider", id);
    assert_eq!(connection["authorization"], "unknown");
    assert_eq!(connection["lifecycle"], "disabled");
    assert_eq!(connection["eligibility"]["state"], "ineligible");
    assert_eq!(connection["eligibility"]["reason"], "connection_disabled");
    assert_eq!(connection["credentialCount"], 1);
    assert_eq!(connection["enabledCredentialCount"], 0);
    harness.stop();
}

#[tokio::test]
async fn d03_connections_keep_two_custom_accounts_with_same_url_separate_and_ordered() {
    let harness = start_loopback("v4-custom-pair").await;
    let shared = "https://same.example/v1/chat/completions";
    let mut ids = Vec::new();
    for name in ["First Custom", "Second Custom"] {
        let (status, created) = send_v3(
            &harness,
            Method::POST,
            "/accounts",
            &cas(
                &harness,
                json!({
                    "providerId": CUSTOM_PROVIDER_ID,
                    "name": name,
                    "key": format!("sk-{name}"),
                    "customConfig": {
                        "endpointUrl": shared,
                        "upstreamProtocol": "chat_completions"
                    },
                    "modelCapabilities": [{
                        "publicModel": format!("{name}-model"),
                        "upstreamModel": "upstream-model",
                        "protocol": "chat_completions"
                    }]
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        ids.push(created["account"]["id"].as_str().unwrap().to_string());
    }
    let (status, body) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let customs: Vec<&Value> = connections_of(&body)
        .iter()
        .filter(|connection| connection["legacy"]["kind"] == "custom_account")
        .collect();
    assert_eq!(customs.len(), 2, "{body}");
    assert_eq!(customs[0]["legacy"]["id"], ids[0]);
    assert_eq!(customs[1]["legacy"]["id"], ids[1]);
    assert_eq!(customs[0]["name"], "First Custom");
    assert_eq!(customs[1]["name"], "Second Custom");
    assert_eq!(customs[0]["endpoints"][0]["url"], shared);
    assert_eq!(customs[1]["endpoints"][0]["url"], shared);
    assert_ne!(customs[0]["id"], customs[1]["id"]);
    harness.stop();
}

#[tokio::test]
async fn connections_omit_builtins_without_accounts() {
    let harness = start_loopback("v4-omit-builtins").await;
    let (status, body) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let builtin_ids: Vec<&str> = connections_of(&body)
        .iter()
        .filter(|connection| connection["legacy"]["kind"] == "builtin_provider")
        .map(|connection| connection["legacy"]["id"].as_str().unwrap())
        .collect();
    for omitted in [
        OPENCODE_PROVIDER_ID,
        COMMAND_CODE_PROVIDER_ID,
        MINIMAX_PROVIDER_ID,
        KIMI_PROVIDER_ID,
        OLLAMA_PROVIDER_ID,
        CUSTOM_PROVIDER_ID,
        CPA_PROVIDER_ID,
    ] {
        assert!(
            !builtin_ids.contains(&omitted),
            "{omitted} must stay a template until it has an account: {builtin_ids:?}"
        );
    }
    harness.stop();
}

#[tokio::test]
async fn connection_ids_are_stable_across_reads_and_equal_domain_derivation() {
    let harness = start_loopback("v4-stable-ids").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Stable Lab",
                "https://stable.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let legacy_id = created["provider"]["id"].as_str().unwrap().to_string();
    let expected = connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &legacy_id);

    let (status, first) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, second) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let first_id = find_legacy(&first, "dynamic_provider", &legacy_id)["id"]
        .as_str()
        .unwrap();
    let second_id = find_legacy(&second, "dynamic_provider", &legacy_id)["id"]
        .as_str()
        .unwrap();
    assert_eq!(first_id, second_id);
    assert_eq!(first_id, expected.as_str());
    harness.stop();
}

#[tokio::test]
async fn v4_listing_makes_zero_outbound_requests() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("v4-no-outbound").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Quiet Lab",
                &format!("{upstream}/v1/chat/completions"),
                "chat_completions",
                "bearer",
                Some("sk-quiet"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, templates) = send_v4(&harness, Method::GET, "/templates", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{templates}");
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "V4 listings must not issue outbound requests"
    );
    harness.stop();
}

#[tokio::test]
async fn v4_requires_session_like_v3() {
    let harness = start_public("v4-session").await;
    for path in ["/contract", "/accounts"] {
        let response = harness
            .client
            .get(format!("{}{path}", v4_base(&harness)))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["code"], "unauthorized", "{path}");
    }
    harness.stop();
}

#[tokio::test]
async fn v4_contract_returns_live_revision() {
    let harness = start_loopback("v4-contract").await;
    let (status, contract) = send_v4(&harness, Method::GET, "/contract", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{contract}");
    assert_eq!(contract["revision"], harness.state.settings_revision());
    assert_eq!(
        contract["processGeneration"],
        harness.state.process_generation()
    );
    assert_eq!(
        contract["pricingRevision"],
        harness.state.pricing_snapshot().revision
    );
    harness.stop();
}

fn commit_new_body(
    name: &str,
    endpoint: &str,
    auth_kind: &str,
    authorization: Option<Value>,
    targets: Value,
) -> Value {
    let mut body = json!({
        "connection": {
            "kind": "new",
            "templateId": "custom-http",
            "name": name,
            "endpointUrl": endpoint,
            "upstreamProtocol": "chat_completions",
            "authKind": auth_kind
        },
        "targets": targets
    });
    if let Some(authorization) = authorization {
        body["authorization"] = authorization;
    }
    body
}

fn default_targets() -> Value {
    json!([{
        "publicModel": "lab-opus",
        "upstreamModel": "vendor/opus"
    }])
}

fn api_key_auth(secret: &str, label: Option<&str>) -> Value {
    let mut auth = json!({
        "kind": "api_key",
        "secretInput": secret
    });
    if let Some(label) = label {
        auth["accountLabel"] = json!(label);
    }
    auth
}

fn operation_id(tag: u16) -> String {
    format!("aaaaaaaa-bbbb-4ccc-8ddd-{tag:012x}")
}

fn commit_cas(harness: &V3Harness, operation_id: &str, patch: Value) -> Value {
    let mut body = cas(harness, patch);
    body["operationId"] = json!(operation_id);
    body
}

fn dynamic_provider_count(harness: &V3Harness) -> usize {
    harness.state.dynamic_providers().len()
}

fn account_count_for(harness: &V3Harness, provider_id: &str) -> i64 {
    harness
        .state
        .db
        .lock()
        .count_accounts_for_provider(provider_id)
        .unwrap()
}

fn first_account_id_for(harness: &V3Harness, provider_id: &str) -> String {
    harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .unwrap_or_else(|| panic!("missing account for {provider_id}"))
}

fn operation_exists(harness: &V3Harness, operation_id: &str) -> bool {
    harness
        .state
        .db
        .lock()
        .find_dashboard_operation(operation_id)
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn commit_new_keyed_connection_with_api_key_creates_provider_and_first_account_atomically() {
    let harness = start_loopback("v4-commit-new").await;
    let operation_id = operation_id(1);
    let secret = "sk-onboard-primary";
    let body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Onboard Lab",
            "https://onboard.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth(secret, Some("Primary"))),
            default_targets(),
        ),
    );
    let before = harness.state.settings_revision();
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_secret_free(&result, &[secret]);
    assert_eq!(result["replayed"], false);
    assert_eq!(result["revision"]["revision"], before + 1);
    assert!(result["credentialId"].as_str().is_some(), "{result}");
    assert!(result["accountId"].as_str().is_some(), "{result}");
    assert_ne!(result["credentialId"], result["accountId"], "{result}");
    assert_eq!(dynamic_provider_count(&harness), 1);
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    let account_id = result["accountId"].as_str().unwrap();
    assert_eq!(
        result["credentialId"],
        credential_id_for_legacy_account(account_id).as_str()
    );
    assert_eq!(
        result["connectionId"],
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &provider_id).as_str()
    );
    assert_eq!(result["targetIds"].as_array().map(Vec::len), Some(1));
    harness.stop();
}

#[tokio::test]
async fn commit_replay_with_same_operation_id_returns_stored_result_and_creates_nothing() {
    let harness = start_loopback("v4-commit-replay").await;
    let operation_id = operation_id(2);
    let secret = "sk-onboard-replay";
    let body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Replay Lab",
            "https://replay.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth(secret, None)),
            default_targets(),
        ),
    );
    let before = harness.state.settings_revision();
    let (status, first) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let after_first = harness.state.settings_revision();
    assert_eq!(after_first, before + 1);
    let (status, second) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["replayed"], true);
    assert_eq!(second["connectionId"], first["connectionId"]);
    assert_eq!(second["credentialId"], first["credentialId"]);
    assert_eq!(second["targetIds"], first["targetIds"]);
    assert_eq!(second["revision"]["revision"], after_first);
    assert_eq!(harness.state.settings_revision(), after_first);
    assert_eq!(dynamic_provider_count(&harness), 1);
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    harness.stop();
}

#[tokio::test]
async fn commit_replay_with_refreshed_cas_tokens_still_replays_and_creates_nothing() {
    let harness = start_loopback("v4-commit-replay-cas").await;
    let operation_id = operation_id(14);
    let secret = "sk-onboard-replay-cas";
    let first_body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Replay CAS Lab",
            "https://replay-cas.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth(secret, None)),
            default_targets(),
        ),
    );
    let (status, first) = send_v4(&harness, Method::POST, "/onboarding/commit", &first_body).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let after_first = harness.state.settings_revision();
    let (status, contract) = send_v4(&harness, Method::GET, "/contract", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{contract}");
    assert_eq!(contract["revision"], after_first);

    let mut retry = first_body;
    retry["expectedRevision"] = contract["revision"].clone();
    retry["processGeneration"] = contract["processGeneration"].clone();
    let (status, second) = send_v4(&harness, Method::POST, "/onboarding/commit", &retry).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert_eq!(second["replayed"], true);
    assert_eq!(second["connectionId"], first["connectionId"]);
    assert_eq!(second["credentialId"], first["credentialId"]);
    assert_eq!(second["revision"]["revision"], after_first);
    assert_eq!(harness.state.settings_revision(), after_first);
    assert_eq!(dynamic_provider_count(&harness), 1);
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    harness.stop();
}

#[tokio::test]
async fn commit_same_operation_id_with_different_payload_is_rejected() {
    let harness = start_loopback("v4-commit-mismatch").await;
    let operation_id = operation_id(3);
    let secret = "sk-onboard-mismatch";
    let original = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Mismatch Lab",
            "https://mismatch.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth(secret, None)),
            default_targets(),
        ),
    );
    let (status, first) = send_v4(&harness, Method::POST, "/onboarding/commit", &original).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let provider_id = harness.state.dynamic_providers()[0].id.clone();

    let mut different_name = original.clone();
    different_name["connection"]["name"] = json!("Other Lab");
    let (status, name_error) = send_v4(
        &harness,
        Method::POST,
        "/onboarding/commit",
        &different_name,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{name_error}");
    assert_eq!(name_error["code"], "operationPayloadMismatch");
    assert!(name_error.get("connectionId").is_none(), "{name_error}");
    assert_ne!(name_error.get("replayed"), Some(&json!(true)));
    assert_secret_free(&name_error, &[secret]);

    let mut different_secret = original.clone();
    different_secret["authorization"]["secretInput"] = json!("sk-other-secret");
    let (status, secret_error) = send_v4(
        &harness,
        Method::POST,
        "/onboarding/commit",
        &different_secret,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{secret_error}");
    assert_eq!(secret_error["code"], "operationPayloadMismatch");
    assert!(secret_error.get("connectionId").is_none(), "{secret_error}");
    assert_secret_free(&secret_error, &[secret, "sk-other-secret"]);

    assert_eq!(dynamic_provider_count(&harness), 1);
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    harness.stop();
}

#[tokio::test]
async fn commit_without_authorization_on_keyed_template_saves_definition_only() {
    let harness = start_loopback("v4-commit-definition").await;
    let operation_id = operation_id(4);
    let body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Definition Lab",
            "https://definition.example/v1/chat/completions",
            "bearer",
            None,
            default_targets(),
        ),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["credentialId"], Value::Null);
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 0);
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = find_legacy(&connections, "dynamic_provider", &provider_id);
    assert_eq!(connection["authorization"], "missing");
    harness.stop();
}

#[tokio::test]
async fn commit_none_auth_template_creates_singleton_and_rejects_api_key() {
    let harness = start_loopback("v4-commit-none").await;
    let rejected = commit_cas(
        &harness,
        &operation_id(5),
        commit_new_body(
            "None Lab",
            "https://none.example/v1/chat/completions",
            "none",
            Some(api_key_auth("sk-should-reject", None)),
            default_targets(),
        ),
    );
    let (status, error) = send_v4(&harness, Method::POST, "/onboarding/commit", &rejected).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "invalidRequest");
    assert_eq!(dynamic_provider_count(&harness), 0);

    let accepted = commit_cas(
        &harness,
        &operation_id(6),
        commit_new_body(
            "None Lab",
            "https://none.example/v1/chat/completions",
            "none",
            Some(json!({ "kind": "none" })),
            default_targets(),
        ),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &accepted).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(result["credentialId"].as_str().is_some(), "{result}");
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    harness.stop();
}

#[tokio::test]
async fn commit_existing_dynamic_connection_adds_second_key() {
    let harness = start_loopback("v4-commit-existing").await;
    let first = commit_cas(
        &harness,
        &operation_id(7),
        commit_new_body(
            "Two Key Lab",
            "https://twokey.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth("sk-first-key", Some("First"))),
            default_targets(),
        ),
    );
    let (status, created) = send_v4(&harness, Method::POST, "/onboarding/commit", &first).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let connection_id = created["connectionId"].as_str().unwrap().to_string();
    let provider_id = harness.state.dynamic_providers()[0].id.clone();

    let second = commit_cas(
        &harness,
        &operation_id(8),
        json!({
            "connection": {
                "kind": "existing",
                "connectionId": connection_id
            },
            "authorization": api_key_auth("sk-second-key", Some("Second")),
            "targets": []
        }),
    );
    let (status, added) = send_v4(&harness, Method::POST, "/onboarding/commit", &second).await;
    assert_eq!(status, StatusCode::OK, "{added}");
    assert_eq!(added["connectionId"], connection_id);
    assert_eq!(added["targetIds"], json!([]));
    assert_ne!(added["credentialId"], created["credentialId"]);
    assert_eq!(account_count_for(&harness, &provider_id), 2);
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = find_legacy(&connections, "dynamic_provider", &provider_id);
    assert_eq!(connection["credentialCount"], 2);
    harness.stop();
}

#[tokio::test]
async fn commit_existing_builtin_or_custom_connection_is_rejected() {
    let harness = start_loopback("v4-commit-reject-legacy").await;
    let builtin_id =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID);
    let builtin = commit_cas(
        &harness,
        &operation_id(9),
        json!({
            "connection": {
                "kind": "existing",
                "connectionId": builtin_id.as_str()
            },
            "authorization": api_key_auth("sk-builtin", None),
            "targets": []
        }),
    );
    let (status, error) = send_v4(&harness, Method::POST, "/onboarding/commit", &builtin).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "invalidRequest");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("builtin and Custom API connections add Keys on Accounts"),
        "{error}"
    );

    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Custom Reject",
                "key": "sk-custom-reject",
                "customConfig": {
                    "endpointUrl": "https://custom-reject.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "custom-model",
                    "upstreamModel": "upstream-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let custom = connections_of(&connections)
        .iter()
        .find(|connection| connection["legacy"]["kind"] == "custom_account")
        .expect("custom connection");
    let custom_commit = commit_cas(
        &harness,
        &operation_id(10),
        json!({
            "connection": {
                "kind": "existing",
                "connectionId": custom["id"]
            },
            "authorization": api_key_auth("sk-custom-second", None),
            "targets": []
        }),
    );
    let (status, error) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &custom_commit).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "invalidRequest");
    harness.stop();
}

#[tokio::test]
async fn commit_cas_conflict_writes_nothing() {
    let harness = start_loopback("v4-commit-cas").await;
    let operation_id = operation_id(11);
    let mut body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "CAS Lab",
            "https://cas.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth("sk-cas", None)),
            default_targets(),
        ),
    );
    body["expectedRevision"] = json!(harness.state.settings_revision() + 99);
    let (status, error) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "revisionConflict");
    assert_eq!(dynamic_provider_count(&harness), 0);
    assert!(!operation_exists(&harness, &operation_id));

    let retry = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "CAS Lab",
            "https://cas.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth("sk-cas", None)),
            default_targets(),
        ),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &retry).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["replayed"], false);
    assert_eq!(dynamic_provider_count(&harness), 1);
    harness.stop();
}

#[tokio::test]
async fn s04_commit_responses_and_operation_rows_are_secret_free() {
    let harness = start_loopback("v4-commit-secret-free").await;
    let secret = "sk-onboard-never-echo";
    let operation_id = operation_id(12);
    let body = commit_cas(
        &harness,
        &operation_id,
        commit_new_body(
            "Secret Free Lab",
            "https://secret-free.example/v1/chat/completions",
            "bearer",
            Some(api_key_auth(secret, None)),
            default_targets(),
        ),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_secret_free(&result, &[secret]);
    let row = harness
        .state
        .db
        .lock()
        .find_dashboard_operation(&operation_id)
        .unwrap()
        .expect("operation row");
    for haystack in [&row.result_json, &row.payload_digest] {
        assert!(!haystack.contains(secret), "secret leaked in {haystack}");
        assert!(
            !haystack.contains("secretInput"),
            "request field leaked in {haystack}"
        );
    }
    harness.stop();
}

#[tokio::test]
async fn commit_makes_zero_outbound_requests() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("v4-commit-no-outbound").await;
    let body = commit_cas(
        &harness,
        &operation_id(13),
        commit_new_body(
            "Quiet Commit",
            &format!("{upstream}/v1/chat/completions"),
            "bearer",
            Some(api_key_auth("sk-quiet-commit", None)),
            default_targets(),
        ),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "onboarding commit must not issue outbound requests"
    );
    harness.stop();
}

fn identities_of(body: &Value) -> &[Value] {
    body["identities"].as_array().expect("identities array")
}

fn find_identity_legacy<'a>(body: &'a Value, kind: &str, id: &str) -> &'a Value {
    identities_of(body)
        .iter()
        .find(|identity| identity["legacy"]["kind"] == kind && identity["legacy"]["id"] == id)
        .unwrap_or_else(|| panic!("missing identity {kind}:{id} in {body}"))
}

#[tokio::test]
async fn identities_project_one_container_one_credential_one_binding_per_account() {
    let harness = start_loopback("v4-identities-shape").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Go Key",
                "key": "sk-go-identity"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let identity = find_identity_legacy(&body, "account", &account_id);
    assert_eq!(identity["credentials"].as_array().unwrap().len(), 1);
    assert_eq!(
        identity["credentials"][0]["bindings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(identity["credentials"][0]["subject"], "account_credential");
    harness.stop();
}

#[tokio::test]
async fn d04_identities_keep_platform_parent_and_linked_key_separate_with_declared_relation() {
    let harness = start_loopback("v4-identities-platform").await;
    let (status, parent) = send_v3(
        &harness,
        Method::POST,
        "/platform-accounts",
        &cas(
            &harness,
            json!({
                "kind": "new_api",
                "name": "Parent",
                "baseUrl": "https://new.example/v1",
                "userCredential": "sk-platform-observer"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    let parent_id = parent["accounts"][0]["id"].as_str().unwrap().to_string();
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Linked Custom",
                "key": "sk-linked-custom",
                "customConfig": {
                    "endpointUrl": "https://old.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "linked-model",
                    "upstreamModel": "linked-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, linked) = send_v3(
        &harness,
        Method::PUT,
        &format!("/accounts/{account_id}/platform-link"),
        &cas(
            &harness,
            json!({
                "platformAccountId": parent_id,
                "group": { "id": "default", "autoGroups": [], "verified": false }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");

    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = find_identity_legacy(&body, "account", &account_id);
    let platform = find_identity_legacy(&body, "platform_account", &parent_id);
    assert_ne!(key["identity"]["id"], platform["identity"]["id"]);
    assert_eq!(key["identity"]["identityConfidence"], "declared");
    assert_eq!(
        key["identity"]["authorityRef"]["issuerOrSite"],
        parent["accounts"][0]["baseUrl"]
    );
    assert_eq!(key["declaredRelations"][0]["platformAccountId"], parent_id);
    assert!(platform["declaredRelations"].as_array().unwrap().is_empty());
    assert_eq!(
        platform["credentials"][0]["credential"]["purpose"],
        "platform_observer"
    );
    assert_eq!(
        platform["credentials"][0]["credential"]["hasMaterial"],
        true
    );
    assert!(
        platform["credentials"][0]["bindings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    harness.stop();
}

#[tokio::test]
async fn d07_identities_omit_subscription_for_dynamic_accounts() {
    let harness = start_loopback("v4-identities-d07").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "No Sub Lab",
                "https://nosub.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                Some("sk-nosub"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, go) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Go With Dates",
                "key": "sk-go-sub"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go}");
    let go_id = go["account"]["id"].as_str().unwrap().to_string();
    let (status, custom) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Custom No Sub",
                "key": "sk-custom-nosub",
                "customConfig": {
                    "endpointUrl": "https://c.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "c-model",
                    "upstreamModel": "c-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = custom["account"]["id"].as_str().unwrap().to_string();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let go_identity = find_identity_legacy(&body, "account", &go_id);
    assert!(
        go_identity["credentials"][0]["subscription"].is_object(),
        "{go_identity}"
    );
    let custom_identity = find_identity_legacy(&body, "account", &custom_id);
    assert!(
        custom_identity["credentials"][0]["subscription"].is_null(),
        "{custom_identity}"
    );
    let dynamic_identity = identities_of(&body)
        .iter()
        .find(|identity| {
            identity["legacy"]["kind"] == "account" && identity["identity"]["label"] == "No Sub Lab"
        })
        .expect("dynamic identity");
    assert!(
        dynamic_identity["credentials"][0]["subscription"].is_null(),
        "{dynamic_identity}"
    );
    assert!(
        dynamic_identity["credentials"][0]["quotaWindows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|window| window["metric"].is_null()),
        "{dynamic_identity}"
    );
    harness.stop();
}

#[tokio::test]
async fn s04_identities_are_secret_free() {
    let harness = start_loopback("v4-identities-secrets").await;
    let secret = "sk-must-not-appear-in-identities";
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Secret Key",
                "key": secret
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &[secret]);
    let identity = find_identity_legacy(&body, "account", account_id);
    let secret_ref = identity["credentials"][0]["credential"]["secretRef"]
        .as_str()
        .unwrap();
    assert_eq!(secret_ref, format!("account:{account_id}"));
    assert!(!secret_ref.contains("cipher"));
    harness.stop();
}

#[tokio::test]
async fn identities_reflect_account_create_and_delete_through_v3() {
    let harness = start_loopback("v4-identities-crud").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Temp Key",
                "key": "sk-temp"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    find_identity_legacy(&body, "account", &account_id);
    let (status, deleted) = send_v3(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(
        identities_of(&after)
            .iter()
            .all(|identity| identity["legacy"]["id"] != account_id),
        "{after}"
    );
    harness.stop();
}

#[tokio::test]
async fn identities_make_zero_outbound_requests() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("v4-identities-no-outbound").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Quiet Identities",
                &format!("{upstream}/v1/chat/completions"),
                "chat_completions",
                "bearer",
                Some("sk-quiet-identities"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "V4 identities must not issue outbound requests"
    );
    harness.stop();
}

#[tokio::test]
async fn identities_redact_last_error_and_omit_when_cipher_is_unreadable() {
    let harness = start_loopback("v4-identities-last-error").await;
    let secret = "sk-must-redact-from-last-error";
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Redact Key",
                "key": secret
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    harness
        .state
        .db
        .lock()
        .set_account_cooldown(
            &account_id,
            Some(Utc::now() + Duration::hours(1)),
            Some(&format!("rate limit echoed {secret}")),
        )
        .unwrap();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &[secret]);
    let identity = find_identity_legacy(&body, "account", &account_id);
    let last_error = identity["credentials"][0]["lastError"]
        .as_str()
        .expect("redacted lastError");
    assert!(last_error.contains("rate limit echoed"), "{last_error}");
    assert!(!last_error.contains(secret), "{last_error}");

    let now = Utc::now();
    let broken = Account {
        id: "unreadable-last-error".into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        credential_kind: default_credential_kind(),
        quota_scope: default_quota_scope(),
        name: "Unreadable".into(),
        username: None,
        password_cipher: None,
        key_cipher: "not-a-valid-ciphertext".into(),
        enabled: false,
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
        last_error: Some(format!("stored error {secret}")),
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    harness.state.db.lock().create_account(&broken).unwrap();
    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_secret_free(&after, &[secret]);
    let unreadable = find_identity_legacy(&after, "account", "unreadable-last-error");
    assert!(
        unreadable["credentials"][0]["lastError"].is_null(),
        "{unreadable}"
    );
    harness.stop();
}

#[tokio::test]
async fn identities_show_invalid_auth_and_exact_cooldown_instant() {
    let harness = start_loopback("v4-identities-auth-cooldown").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Cooling Key",
                "key": "sk-cooling"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    let until = Utc::now() + Duration::hours(5);
    harness
        .state
        .db
        .lock()
        .set_account_auth_error(&account_id, Some("auth failed"))
        .unwrap();
    harness
        .state
        .db
        .lock()
        .set_account_cooldown(&account_id, Some(until), Some("cooling"))
        .unwrap();
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let identity = find_identity_legacy(&body, "account", &account_id);
    assert_eq!(
        identity["credentials"][0]["credential"]["authState"],
        "invalid"
    );
    let windows = identity["credentials"][0]["quotaWindows"]
        .as_array()
        .unwrap();
    let generic = windows
        .iter()
        .find(|window| window["period"] == "generic")
        .expect("generic cooldown window");
    let blocked = chrono::DateTime::parse_from_rfc3339(
        generic["blockedUntil"].as_str().expect("blockedUntil"),
    )
    .unwrap()
    .with_timezone(&Utc);
    assert_eq!(blocked, until);
    harness.stop();
}

#[tokio::test]
async fn identities_project_zen_free_as_anonymous_with_stored_binding_id() {
    let harness = start_loopback("v4-identities-zen-free").await;
    let (status, body) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let zen = find_identity_legacy(&body, "account", ZEN_FREE_ACCOUNT_ID);
    assert_eq!(zen["credentials"][0]["subject"], "anonymous");
    let connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
    );
    assert_eq!(
        zen["credentials"][0]["bindings"][0]["id"],
        anonymous_binding_id_for(&connection).as_str()
    );
    harness.stop();
}

fn credential_of<'a>(body: &'a Value, kind: &str, id: &str) -> &'a Value {
    &find_identity_legacy(body, kind, id)["credentials"][0]
}

async fn create_go_account(harness: &V3Harness, name: &str, key: &str) -> String {
    let (status, created) = send_v3(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": name,
                "key": key
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["account"]["id"].as_str().unwrap().to_string();
    harness.enable_account(&id);
    id
}

#[tokio::test]
async fn rotate_increments_version_keeps_ids_and_clears_old_auth_error() {
    let harness = start_loopback("v4-rotate-d06").await;
    let secret = "sk-rotate-d06";
    let account_id = create_go_account(&harness, "Rotate D06", "sk-original-d06").await;
    harness
        .state
        .db
        .lock()
        .set_account_auth_error(&account_id, Some("stale auth"))
        .unwrap();
    harness
        .state
        .db
        .lock()
        .set_account_cooldown(
            &account_id,
            Some(Utc::now() + Duration::hours(1)),
            Some("stale last error"),
        )
        .unwrap();
    let (status, before) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    let identity = find_identity_legacy(&before, "account", &account_id);
    let credential = &identity["credentials"][0];
    assert_eq!(credential["credential"]["authState"], "invalid");
    let credential_id = credential["credential"]["id"].as_str().unwrap().to_string();
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let binding_id = credential["bindings"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let version = credential["credential"]["version"].as_u64().unwrap();
    let auth_state_version = credential["credential"]["authStateVersion"]
        .as_u64()
        .unwrap();

    let (status, result) = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{credential_id}/rotate"),
        &cas(&harness, json!({ "secretInput": secret })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["credentialId"], credential_id);
    assert_eq!(result["version"], version + 1);
    assert_eq!(result["authStateVersion"], auth_state_version + 1);
    assert_eq!(result["replayed"], false);

    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = find_identity_legacy(&after, "account", &account_id);
    let credential = &identity["credentials"][0];
    assert_eq!(identity["identity"]["id"], identity_id);
    assert_eq!(credential["credential"]["id"], credential_id);
    assert_eq!(credential["bindings"][0]["id"], binding_id);
    assert_eq!(credential["credential"]["version"], version + 1);
    assert_eq!(
        credential["credential"]["authStateVersion"],
        auth_state_version + 1
    );
    assert_eq!(credential["credential"]["authState"], "unknown");
    assert!(credential["lastError"].is_null(), "{credential}");
    let stored = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .expect("account");
    assert!(stored.auth_error.is_none());
    assert!(stored.last_error.is_none());
    assert_eq!(
        harness.state.decrypt_key(&stored.key_cipher).unwrap(),
        secret
    );
    harness.stop();
}

#[tokio::test]
async fn rotate_does_not_create_a_second_account_or_binding() {
    let harness = start_loopback("v4-rotate-no-second").await;
    let account_id = create_go_account(&harness, "Rotate Once", "sk-once").await;
    let counts = |harness: &V3Harness| {
        let db = harness.state.db.lock();
        let accounts = db.list_accounts().unwrap().len();
        let snapshot = db.list_identity_model().unwrap();
        (
            accounts,
            snapshot.identities.len(),
            snapshot.accounts.len(),
            snapshot
                .accounts
                .iter()
                .map(|record| record.binding_id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            snapshot
                .accounts
                .iter()
                .map(|record| record.credential_id.clone())
                .collect::<std::collections::BTreeSet<_>>(),
        )
    };
    let before = counts(&harness);
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let credential_id = credential_of(&listed, "account", &account_id)["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, result) = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{credential_id}/rotate"),
        &cas(&harness, json!({ "secretInput": "sk-once-rotated" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(counts(&harness), before);
    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = find_identity_legacy(&after, "account", &account_id);
    assert_eq!(identity["credentials"].as_array().unwrap().len(), 1);
    assert_eq!(
        identity["credentials"][0]["bindings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    harness.stop();
}

#[tokio::test]
async fn rotate_unknown_or_observer_or_anonymous_is_rejected() {
    let harness = start_loopback("v4-rotate-rejected").await;
    let (status, parent) = send_v3(
        &harness,
        Method::POST,
        "/platform-accounts",
        &cas(
            &harness,
            json!({
                "kind": "new_api",
                "name": "Observer Parent",
                "baseUrl": "https://observer.example/v1",
                "userCredential": "sk-observer"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    let parent_id = parent["accounts"][0]["id"].as_str().unwrap().to_string();
    let observer_id = observer_credential_id_for_platform_account(&parent_id).to_string();
    let anonymous_id = credential_id_for_legacy_account(ZEN_FREE_ACCOUNT_ID).to_string();
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "No Auth Rotate",
                "https://none-rotate.example/v1/chat/completions",
                "chat_completions",
                "none",
                None,
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let no_auth_account = identities_of(&listed)
        .iter()
        .find(|identity| {
            identity["legacy"]["kind"] == "account"
                && identity["identity"]["label"] == "No Auth Rotate"
        })
        .expect("no-auth identity");
    let no_auth_id = no_auth_account["credentials"][0]["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let unknown = send_v4(
        &harness,
        Method::POST,
        "/credentials/00000000-0000-0000-0000-000000000000/rotate",
        &cas(&harness, json!({ "secretInput": "sk-unused" })),
    )
    .await;
    assert_eq!(unknown.0, StatusCode::NOT_FOUND, "{}", unknown.1);
    assert_eq!(unknown.1["code"], "notFound");

    let observer = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{observer_id}/rotate"),
        &cas(&harness, json!({ "secretInput": "sk-unused" })),
    )
    .await;
    assert_eq!(observer.0, StatusCode::BAD_REQUEST, "{}", observer.1);
    assert_eq!(observer.1["code"], "invalidRequest");
    assert!(
        observer.1["message"]
            .as_str()
            .unwrap()
            .contains("platform observer"),
        "{}",
        observer.1
    );

    let anonymous = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{anonymous_id}/rotate"),
        &cas(&harness, json!({ "secretInput": "sk-unused" })),
    )
    .await;
    assert_eq!(anonymous.0, StatusCode::BAD_REQUEST, "{}", anonymous.1);
    assert_eq!(anonymous.1["code"], "invalidRequest");

    let no_auth = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{no_auth_id}/rotate"),
        &cas(&harness, json!({ "secretInput": "sk-unused" })),
    )
    .await;
    assert_eq!(no_auth.0, StatusCode::BAD_REQUEST, "{}", no_auth.1);
    assert_eq!(no_auth.1["code"], "invalidRequest");
    harness.stop();
}

#[tokio::test]
async fn rotate_cas_conflict_writes_nothing() {
    let harness = start_loopback("v4-rotate-cas").await;
    let account_id = create_go_account(&harness, "Rotate CAS", "sk-cas-original").await;
    let before = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .expect("account");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let credential = credential_of(&listed, "account", &account_id);
    let versions = (
        credential["credential"]["version"].as_u64().unwrap(),
        credential["credential"]["authStateVersion"]
            .as_u64()
            .unwrap(),
    );
    let credential_id = credential["credential"]["id"].as_str().unwrap().to_string();
    let mut body = cas(&harness, json!({ "secretInput": "sk-cas-rotated" }));
    body["expectedRevision"] = json!(harness.state.settings_revision() + 99);
    let (status, error) = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{credential_id}/rotate"),
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "revisionConflict");
    let after = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .expect("account");
    assert_eq!(after.key_cipher, before.key_cipher);
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let credential = credential_of(&listed, "account", &account_id);
    assert_eq!(
        (
            credential["credential"]["version"].as_u64().unwrap(),
            credential["credential"]["authStateVersion"]
                .as_u64()
                .unwrap(),
        ),
        versions
    );
    harness.stop();
}

#[tokio::test]
async fn rotate_response_is_secret_free() {
    let harness = start_loopback("v4-rotate-secret-free").await;
    let secret = "sk-rotate-never-echo";
    let account_id = create_go_account(&harness, "Rotate Secret", "sk-before-secret").await;
    let credential_id = credential_id_for_legacy_account(&account_id).to_string();
    let (status, result) = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{credential_id}/rotate"),
        &cas(&harness, json!({ "secretInput": secret })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_secret_free(&result, &[secret, "sk-before-secret"]);
    assert!(result.get("secretInput").is_none());
    harness.stop();
}

#[tokio::test]
async fn rotate_makes_zero_outbound_requests() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("v4-rotate-no-outbound").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Quiet Rotate",
                &format!("{upstream}/v1/chat/completions"),
                "chat_completions",
                "bearer",
                Some("sk-quiet-rotate"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let credential_id = identities_of(&listed)
        .iter()
        .find(|identity| {
            identity["legacy"]["kind"] == "account"
                && identity["identity"]["label"] == "Quiet Rotate"
        })
        .expect("dynamic identity")["credentials"][0]["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, result) = send_v4(
        &harness,
        Method::POST,
        &format!("/credentials/{credential_id}/rotate"),
        &cas(&harness, json!({ "secretInput": "sk-quiet-rotated" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "credential rotate must not issue outbound requests"
    );
    harness.stop();
}

#[tokio::test]
async fn d05_second_product_reuses_identity_and_joins_declared_pool() {
    let harness = start_loopback("v4-d05-second-product").await;
    let account_id = create_go_account(&harness, "Plan Key", "sk-plan-d05").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_id);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let first_credential = identity["credentials"][0]["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_binding = identity["credentials"][0]["bindings"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "API Product",
                "https://api-d05.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                Some("sk-api-other-identity"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let api_connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|connection| connection["name"] == "API Product")
        .expect("api connection")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_connection =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, OPENCODE_PROVIDER_ID)
            .to_string();
    assert_ne!(api_connection, plan_connection);

    let (status, result) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": api_connection,
                "secretInput": "sk-plan-api-d05",
                "quotaSharing": { "kind": "shared", "credentialId": first_credential }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["identityId"], identity_id);
    assert_eq!(result["connectionId"], api_connection);
    assert_eq!(result["replayed"], false);
    assert_secret_free(
        &result,
        &["sk-plan-api-d05", "sk-plan-d05", "sk-api-other-identity"],
    );

    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = identities_of(&after)
        .iter()
        .find(|item| item["identity"]["id"] == identity_id)
        .expect("shared identity");
    assert_eq!(identity["credentials"].as_array().unwrap().len(), 2);
    let connection_ids: Vec<_> = identity["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|credential| credential["bindings"][0]["connectionId"].as_str().unwrap())
        .collect();
    assert!(connection_ids.contains(&plan_connection.as_str()));
    assert!(connection_ids.contains(&api_connection.as_str()));
    assert_ne!(result["credentialId"], first_credential);
    assert_ne!(result["bindingId"], first_binding);

    let members = harness
        .state
        .db
        .lock()
        .shared_pool_account_ids(&account_id)
        .unwrap();
    assert!(members.contains(&account_id));
    assert!(members.contains(&result["accountId"].as_str().unwrap().to_string()));
    assert_eq!(members.len(), 2);
    harness.stop();
}

#[tokio::test]
async fn d05_bindings_enable_independently_and_cas_miss_writes_nothing() {
    let harness = start_loopback("v4-d05-binding-patch").await;
    let account_id = create_go_account(&harness, "Scope A", "sk-scope-a").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_id);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let first_binding = identity["credentials"][0]["bindings"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, second) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-scope-b"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_binding = second["bindingId"].as_str().unwrap().to_string();

    let (status, patched) = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{second_binding}"),
        &cas(
            &harness,
            json!({
                "enabled": false,
                "modelScope": { "kind": "only", "models": ["glm-5.1"] }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["binding"]["id"], second_binding);
    assert_eq!(patched["binding"]["enabled"], false);
    assert_eq!(patched["binding"]["modelScope"]["kind"], "only");

    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = identities_of(&after)
        .iter()
        .find(|item| item["identity"]["id"] == identity_id)
        .expect("identity");
    let bindings: Vec<&Value> = identity["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|credential| &credential["bindings"][0])
        .collect();
    let first = bindings
        .iter()
        .find(|binding| binding["id"] == first_binding)
        .unwrap();
    let second = bindings
        .iter()
        .find(|binding| binding["id"] == second_binding)
        .unwrap();
    assert_eq!(first["enabled"], true);
    assert_eq!(first["modelScope"]["kind"], "all");
    assert_eq!(second["enabled"], false);
    assert_eq!(second["modelScope"]["kind"], "only");

    let mut stale = cas(&harness, json!({ "enabled": true }));
    stale["expectedRevision"] = json!(harness.state.settings_revision() + 99);
    let (status, error) = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{second_binding}"),
        &stale,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "revisionConflict");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let still = identities_of(&listed)
        .iter()
        .find(|item| item["identity"]["id"] == identity_id)
        .expect("identity")["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|credential| credential["bindings"][0]["id"] == second_binding)
        .unwrap();
    assert_eq!(still["bindings"][0]["enabled"], false);
    harness.stop();
}

#[tokio::test]
async fn d02_exhausting_shared_pool_via_key_a_blocks_key_b() {
    let harness = start_loopback("v4-d02-shared-quota").await;
    let account_a = create_go_account(&harness, "Quota A", "sk-quota-a").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_a);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, second) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-quota-b",
                "quotaSharing": {
                    "kind": "shared",
                    "credentialId": identity["credentials"][0]["credential"]["id"]
                }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let account_b = second["accountId"].as_str().unwrap().to_string();
    let until = Utc::now() + Duration::hours(3);
    harness
        .state
        .db
        .lock()
        .set_account_rate_limit(&account_a, until, "429 pool empty", None)
        .unwrap();
    let stored_b = harness
        .state
        .db
        .lock()
        .get_account(&account_b)
        .unwrap()
        .expect("key b");
    assert_eq!(stored_b.cooldown_generic_until, Some(until));
    assert!(stored_b.is_cooling_for(ocg_core::models::UpstreamChannel::Go, Utc::now()));
    harness.stop();
}

#[tokio::test]
async fn binding_and_second_credential_reject_meaningless_targets() {
    let harness = start_loopback("v4-d-class-rejects").await;
    let zen_binding = {
        let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        find_identity_legacy(&listed, "account", ZEN_FREE_ACCOUNT_ID)["credentials"][0]["bindings"]
            [0]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let unknown = send_v4(
        &harness,
        Method::PATCH,
        "/bindings/00000000-0000-0000-0000-000000000000",
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(unknown.0, StatusCode::NOT_FOUND, "{}", unknown.1);
    let zen = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{zen_binding}"),
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(zen.0, StatusCode::BAD_REQUEST, "{}", zen.1);

    let account_id = create_go_account(&harness, "Reject Host", "sk-reject-host").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity_id = find_identity_legacy(&listed, "account", &account_id)["identity"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let zen_connection = connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
    )
    .to_string();
    let rejected = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": zen_connection,
                "secretInput": "sk-unused"
            }),
        ),
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST, "{}", rejected.1);
    harness.stop();
}

#[tokio::test]
async fn saved_grants_are_facts_and_url_edits_do_not_expand_them() {
    let harness = start_loopback("v4-grants-are-facts").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Grant Lab",
                "https://lab.example/v1/chat/completions",
                "chat_completions",
                "bearer",
                Some("sk-grant-lab"),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = identities_of(&listed)
        .iter()
        .find(|item| item["identity"]["label"] == "Grant Lab")
        .expect("lab identity");
    let binding = &identity["credentials"][0]["bindings"][0];
    let binding_id = binding["id"].as_str().unwrap().to_string();
    let original_ids = binding["allowedEndpointIds"].clone();
    let original_origins = binding["allowedOrigins"].clone();
    assert!(!original_ids.as_array().unwrap().is_empty());
    assert_eq!(
        original_origins.as_array().unwrap()[0],
        "https://lab.example"
    );

    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let mut runtime = harness
        .state
        .dynamic_providers()
        .iter()
        .find(|runtime| runtime.id == provider_id)
        .cloned()
        .expect("dynamic provider");
    runtime.endpoint_url = "https://other.example/v1/chat/completions".into();
    harness
        .state
        .db
        .lock()
        .replace_dynamic_provider(&runtime, false, false, None)
        .unwrap();
    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let still = identities_of(&after)
        .iter()
        .find(|item| item["identity"]["label"] == "Grant Lab")
        .expect("lab identity")["credentials"][0]["bindings"][0]
        .clone();
    assert_eq!(still["allowedEndpointIds"], original_ids);
    assert_eq!(still["allowedOrigins"], original_origins);

    let (status, patched) = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{binding_id}"),
        &cas(
            &harness,
            json!({
                "allowedEndpointIds": [],
                "allowedOrigins": []
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["binding"]["allowedEndpointIds"], json!([]));
    assert_eq!(patched["binding"]["allowedOrigins"], json!([]));

    let (status, rejected) = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{binding_id}"),
        &cas(
            &harness,
            json!({
                "allowedEndpointIds": ["not-an-endpoint"],
                "allowedOrigins": ["https://lab.example"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let still = identities_of(&listed)
        .iter()
        .find(|item| item["identity"]["label"] == "Grant Lab")
        .expect("lab identity")["credentials"][0]["bindings"][0]
        .clone();
    assert_eq!(still["allowedEndpointIds"], json!([]));
    harness.stop();
}

#[tokio::test]
async fn independent_credentials_do_not_fanout_cooldown_and_cross_identity_share_is_rejected() {
    let harness = start_loopback("v4-independent-quota").await;
    let account_a = create_go_account(&harness, "Quota A", "sk-quota-a").await;
    let account_other = create_go_account(&harness, "Other Identity", "sk-other").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_a);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let other_credential = find_identity_legacy(&listed, "account", &account_other)["credentials"]
        [0]["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, second) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-quota-b"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let account_b = second["accountId"].as_str().unwrap().to_string();
    let until = Utc::now() + Duration::hours(3);
    harness
        .state
        .db
        .lock()
        .set_account_rate_limit(&account_a, until, "429 pool empty", None)
        .unwrap();
    let stored_b = harness
        .state
        .db
        .lock()
        .get_account(&account_b)
        .unwrap()
        .expect("key b");
    assert!(stored_b.cooldown_generic_until.is_none());

    let rejected = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-cross",
                "quotaSharing": { "kind": "shared", "credentialId": other_credential }
            }),
        ),
    )
    .await;
    assert_eq!(rejected.0, StatusCode::BAD_REQUEST, "{}", rejected.1);
    harness.stop();
}

#[tokio::test]
async fn credential_create_operation_id_is_idempotent() {
    let harness = start_loopback("v4-credential-operation").await;
    let account_id = create_go_account(&harness, "Op Key", "sk-op-a").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_id);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();
    let operation_id = operation_id(21);
    let body = cas(
        &harness,
        json!({
            "connectionId": plan_connection,
            "secretInput": "sk-op-b",
            "operationId": operation_id
        }),
    );
    let (status, first) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["replayed"], false);
    let (status, replay) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["accountId"], first["accountId"]);
    assert_eq!(replay["credentialId"], first["credentialId"]);
    let mut different = body.clone();
    different["secretInput"] = json!("sk-op-other");
    let (status, mismatch) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &different,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{mismatch}");
    assert_eq!(mismatch["code"], "operationPayloadMismatch");
    harness.stop();
}

#[tokio::test]
async fn custom_account_create_captures_grants_once_after_config() {
    let harness = start_loopback("v4-custom-grants-once").await;
    let endpoint = "https://grant.example/v1/chat/completions";
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Grant Custom",
                "key": "sk-grant-custom",
                "customConfig": {
                    "endpointUrl": endpoint,
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "grant-model",
                    "upstreamModel": "grant-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_id);
    let binding = &identity["credentials"][0]["bindings"][0];
    let ids = binding["allowedEndpointIds"].as_array().unwrap();
    assert_eq!(ids.len(), 1, "{binding}");
    assert!(!ids[0].as_str().unwrap().is_empty());
    assert_eq!(binding["allowedOrigins"], json!(["https://grant.example"]));

    let (status, patched) = send_v4(
        &harness,
        Method::PATCH,
        &format!("/bindings/{}", binding["id"].as_str().unwrap()),
        &cas(
            &harness,
            json!({
                "allowedEndpointIds": ids,
                "allowedOrigins": [endpoint]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["binding"]["allowedOrigins"],
        json!(["https://grant.example"])
    );
    harness.stop();
}

#[tokio::test]
async fn quota_pool_id_is_projected_without_cooldown_for_independent_and_shared_members() {
    let harness = start_loopback("v4-quota-pool-id").await;
    let account_a = create_go_account(&harness, "Pool Host", "sk-pool-a").await;
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = find_identity_legacy(&listed, "account", &account_a);
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let first_credential = identity["credentials"][0]["credential"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_pool = identity["credentials"][0]["quotaPoolId"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(!first_pool.is_empty());
    assert!(
        identity["credentials"][0]["quotaWindows"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, independent) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-pool-independent"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{independent}");
    let independent_id = independent["accountId"].as_str().unwrap().to_string();

    let (status, shared) = send_v4(
        &harness,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &harness,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-pool-shared",
                "quotaSharing": { "kind": "shared", "credentialId": first_credential }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shared}");
    let shared_id = shared["accountId"].as_str().unwrap().to_string();

    let (status, after) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = identities_of(&after)
        .iter()
        .find(|item| item["identity"]["id"] == identity_id)
        .expect("identity");
    let by_legacy: std::collections::HashMap<String, &Value> = identity["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|credential| {
            (
                credential["legacy"]["id"].as_str().unwrap().to_string(),
                credential,
            )
        })
        .collect();
    assert_eq!(
        by_legacy[&account_a]["quotaPoolId"].as_str().unwrap(),
        first_pool
    );
    assert!(by_legacy[&independent_id]["quotaPoolId"].is_null());
    assert_eq!(
        by_legacy[&shared_id]["quotaPoolId"].as_str().unwrap(),
        first_pool
    );
    for credential in by_legacy.values() {
        assert!(credential["quotaWindows"].as_array().unwrap().is_empty());
        assert!(!credential["credential"]["id"].as_str().unwrap().is_empty());
    }
    harness.stop();
}

const CHAT_OK: &str = r#"{"id":"ok","object":"chat.completion","model":"vendor/opus","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;

async fn listed_gateway_model_ids(harness: &V3Harness) -> Vec<String> {
    let models = harness
        .client
        .get(format!(
            "http://127.0.0.1:{}/v1/models",
            harness.handle.port
        ))
        .bearer_auth(harness.state.config().gateway_key)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    models["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect()
}

async fn chat_completion(harness: &V3Harness, model: &str) -> (StatusCode, String) {
    let response = harness
        .client
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            harness.handle.port
        ))
        .bearer_auth(harness.state.config().gateway_key)
        .json(&json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1
        }))
        .send()
        .await
        .unwrap();
    (response.status(), response.text().await.unwrap())
}

fn control_plane_draft_ids(harness: &V3Harness) -> Vec<String> {
    harness
        .state
        .db
        .lock()
        .onboarding_draft_provider_ids()
        .unwrap()
        .into_iter()
        .collect()
}

#[tokio::test]
async fn draft_commit_allows_empty_targets_and_stays_off_runtime() {
    let harness = start_loopback("v4-draft-empty").await;
    let operation_id = operation_id(40);
    let body = commit_cas(
        &harness,
        &operation_id,
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Empty Draft",
                "endpointUrl": "https://draft-empty.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "targets": []
        }),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["credentialId"], Value::Null);
    assert_eq!(result["accountId"], Value::Null);
    assert_eq!(dynamic_provider_count(&harness), 0);
    let draft_ids = control_plane_draft_ids(&harness);
    assert_eq!(draft_ids.len(), 1);
    let provider_id = draft_ids[0].clone();
    let reopened = ocg_core::db::Database::open(harness.dir.clone()).unwrap();
    assert!(
        reopened
            .list_dynamic_providers()
            .unwrap()
            .iter()
            .all(|runtime| runtime.id != provider_id)
    );
    assert!(
        reopened
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|runtime| runtime.id == provider_id)
    );
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = find_legacy(&connections, "dynamic_provider", &provider_id);
    assert_eq!(connection["lifecycle"], "draft");
    assert_eq!(connection["eligibility"]["state"], "ineligible");
    let (status, catalog) = send_v3(&harness, Method::GET, "/providers", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let catalog_ids: Vec<&str> = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["providerId"].as_str())
        .collect();
    assert!(!catalog_ids.contains(&provider_id.as_str()), "{catalog}");
    let gateway_key = harness.state.config().gateway_key.clone();
    let models = harness
        .client
        .get(format!(
            "http://127.0.0.1:{}/v1/models",
            harness.handle.port
        ))
        .bearer_auth(gateway_key)
        .send()
        .await
        .unwrap();
    let models_status = models.status();
    let models_body = models.json::<Value>().await.unwrap_or(Value::Null);
    assert_eq!(models_status, StatusCode::OK, "{models_body}");
    let model_text = models_body.to_string();
    assert!(
        !model_text.contains(&provider_id),
        "gateway models leaked draft {models_body}"
    );
    harness.stop();
}

#[tokio::test]
async fn explicit_draft_with_key_and_models_stays_nonsend() {
    let secret = "sk-draft-keyed";
    let mut replies = HashMap::new();
    replies.insert(
        secret.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("v4-draft-keyed").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let endpoint = format!("{upstream}/v1/chat/completions");
    let public_model = "lab-opus";
    let body = commit_cas(
        &harness,
        &operation_id(41),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Keyed Draft",
                "endpointUrl": endpoint,
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "authorization": api_key_auth(secret, Some("Draft Key")),
            "targets": [{
                "publicModel": public_model,
                "upstreamModel": "vendor/opus"
            }]
        }),
    );
    let (status, result) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_secret_free(&result, &[secret]);
    assert_eq!(dynamic_provider_count(&harness), 0);
    let provider_id = control_plane_draft_ids(&harness)[0].clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = find_legacy(&connections, "dynamic_provider", &provider_id);
    assert_eq!(connection["lifecycle"], "draft");
    assert_eq!(connection["eligibility"]["state"], "ineligible");
    assert_eq!(connection["targetCount"], 1);
    assert_eq!(connection["credentialCount"], 1);

    let (status, catalog) = send_v3(&harness, Method::GET, "/providers", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let catalog_ids: Vec<&str> = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["providerId"].as_str())
        .collect();
    assert!(!catalog_ids.contains(&provider_id.as_str()), "{catalog}");
    let listed = listed_gateway_model_ids(&harness).await;
    assert!(
        !listed.iter().any(|id| id == public_model),
        "draft public alias leaked into /v1/models: {listed:?}"
    );
    let (chat_status, chat_body) = chat_completion(&harness, public_model).await;
    assert_ne!(chat_status, StatusCode::OK, "{chat_body}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "draft must not send outbound gateway requests"
    );

    let reopened = ocg_core::db::Database::open(harness.dir.clone()).unwrap();
    assert!(
        reopened
            .list_dynamic_providers()
            .unwrap()
            .iter()
            .all(|runtime| runtime.id != provider_id)
    );
    drop(reopened);
    let listed_after = listed_gateway_model_ids(&harness).await;
    assert!(
        !listed_after.iter().any(|id| id == public_model),
        "restarted snapshot leaked draft alias: {listed_after:?}"
    );
    let (chat_status, chat_body) = chat_completion(&harness, public_model).await;
    assert_ne!(chat_status, StatusCode::OK, "{chat_body}");
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "reopen must not activate a draft send"
    );

    let complete = commit_cas(
        &harness,
        &operation_id(50),
        json!({
            "mode": "complete",
            "connection": {
                "kind": "existing",
                "connectionId": result["connectionId"],
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Keyed Draft",
                    "endpointUrl": endpoint,
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": [{
                "publicModel": public_model,
                "upstreamModel": "vendor/opus"
            }]
        }),
    );
    let (status, completed) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &complete).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(dynamic_provider_count(&harness), 1);
    let account_id = result["accountId"].as_str().expect("draft account id");
    harness.enable_account(account_id);
    let listed_live = listed_gateway_model_ids(&harness).await;
    assert!(
        listed_live.iter().any(|id| id == public_model),
        "completed connection missing public alias: {listed_live:?}"
    );
    let (chat_status, chat_body) = chat_completion(&harness, public_model).await;
    assert_eq!(chat_status, StatusCode::OK, "{chat_body}");
    assert_eq!(calls.lock().expect("fake call log").len(), 1);
    harness.stop();
}

#[tokio::test]
async fn complete_retains_saved_key_when_secret_blank() {
    let harness = start_loopback("v4-draft-retain-key").await;
    let secret = "sk-draft-retain";
    let first = commit_cas(
        &harness,
        &operation_id(51),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Retain Draft",
                "endpointUrl": "https://retain-draft.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "authorization": api_key_auth(secret, Some("Kept")),
            "targets": default_targets()
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &first).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let connection_id = drafted["connectionId"].as_str().unwrap().to_string();
    let credential_id = drafted["credentialId"].as_str().unwrap().to_string();
    let account_id = drafted["accountId"].as_str().unwrap().to_string();
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let before = credential_of(&listed, "account", &account_id);
    let identity_id = find_identity_legacy(&listed, "account", &account_id)["identity"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let binding_id = before["bindings"][0]["id"].as_str().unwrap().to_string();
    let complete = commit_cas(
        &harness,
        &operation_id(52),
        json!({
            "mode": "complete",
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Retain Draft",
                    "endpointUrl": "https://retain-draft.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "authorization": api_key_auth("", Some("Kept")),
            "targets": default_targets()
        }),
    );
    let (status, completed) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &complete).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["credentialId"], credential_id);
    assert_eq!(completed["accountId"], account_id);
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let after = credential_of(&listed, "account", &account_id);
    assert_eq!(after["credential"]["id"], credential_id);
    assert_eq!(after["bindings"][0]["id"], binding_id);
    assert_eq!(
        find_identity_legacy(&listed, "account", &account_id)["identity"]["id"],
        identity_id
    );
    harness.stop();
}

#[tokio::test]
async fn auth_switch_requires_key_in_draft_and_complete_and_keeps_ids() {
    let harness = start_loopback("v4-draft-auth-switch").await;
    let draft = commit_cas(
        &harness,
        &operation_id(53),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Auth Switch",
                "endpointUrl": "http://127.0.0.1:9/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "none"
            },
            "targets": []
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &draft).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let connection_id = drafted["connectionId"].as_str().unwrap().to_string();
    let provider_id = control_plane_draft_ids(&harness)[0].clone();
    let complete = commit_cas(
        &harness,
        &operation_id(54),
        json!({
            "mode": "complete",
            "authorizeCurrentEndpoint": true,
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Auth Switch",
                    "endpointUrl": "http://127.0.0.1:9/v1",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": default_targets()
        }),
    );
    for intent in ["draft", "complete"] {
        let mut attempted = complete.clone();
        attempted["mode"] = json!(intent);
        let (status, error) =
            send_v4(&harness, Method::POST, "/onboarding/commit", &attempted).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{intent}: {error}");
        assert_eq!(error["code"], "invalidRequest");
        assert_eq!(
            harness
                .state
                .db
                .lock()
                .get_dynamic_provider(&provider_id)
                .unwrap()
                .unwrap()
                .auth_kind,
            ocg_domain::dynamic::DynamicAuthKind::None
        );
    }
    assert_eq!(dynamic_provider_count(&harness), 0);
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .provider_is_onboarding_draft(&provider_id)
            .unwrap(),
        Some(true)
    );
    let (status, connections) = send_v4(&harness, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = find_legacy(&connections, "dynamic_provider", &provider_id);
    assert_eq!(connection["lifecycle"], "draft");
    let before = harness.state.db.lock().list_identity_model().unwrap();
    let before = before
        .accounts
        .iter()
        .find(|row| row.account.provider_id == provider_id)
        .unwrap();
    let mut with_key = complete;
    with_key["authorization"] = api_key_auth("sk-synthetic-auth-switch", None);
    let (status, completed) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &with_key).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["accountId"], drafted["accountId"]);
    assert_eq!(completed["credentialId"], drafted["credentialId"]);
    let after = harness.state.db.lock().list_identity_model().unwrap();
    let after = after
        .accounts
        .iter()
        .find(|row| row.account.provider_id == provider_id)
        .unwrap();
    assert_eq!(after.identity_id, before.identity_id);
    assert_eq!(after.binding_id, before.binding_id);
    assert_eq!(
        after.account.credential_kind,
        ocg_domain::catalog::CredentialKind::ApiKey
    );
    assert!(after.has_key_material);
    harness.stop();
}

#[tokio::test]
async fn resume_complete_keeps_same_ids_and_replays() {
    let harness = start_loopback("v4-draft-resume").await;
    let secret = "sk-draft-resume";
    let first = commit_cas(
        &harness,
        &operation_id(42),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Resume Draft",
                "endpointUrl": "https://resume-draft.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "authorization": api_key_auth(secret, Some("Kept")),
            "targets": default_targets()
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &first).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let connection_id = drafted["connectionId"].as_str().unwrap().to_string();
    let credential_id = drafted["credentialId"].as_str().unwrap().to_string();
    let account_id = drafted["accountId"].as_str().unwrap().to_string();
    let provider_id = control_plane_draft_ids(&harness)[0].clone();
    let complete = commit_cas(
        &harness,
        &operation_id(43),
        json!({
            "mode": "complete",
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Resume Draft",
                    "endpointUrl": "https://resume-draft.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": default_targets()
        }),
    );
    let (status, completed) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &complete).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    assert_eq!(completed["connectionId"], connection_id);
    assert_eq!(completed["credentialId"], credential_id);
    assert_eq!(completed["accountId"], account_id);
    assert_eq!(dynamic_provider_count(&harness), 1);
    assert_eq!(harness.state.dynamic_providers()[0].id, provider_id);
    assert!(control_plane_draft_ids(&harness).is_empty());
    assert_eq!(account_count_for(&harness, &provider_id), 1);
    let (status, replayed) = send_v4(&harness, Method::POST, "/onboarding/commit", &complete).await;
    assert_eq!(status, StatusCode::OK, "{replayed}");
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["credentialId"], credential_id);
    assert_eq!(replayed["accountId"], account_id);
    harness.stop();
}

#[tokio::test]
async fn missing_key_complete_fails_atomically() {
    let harness = start_loopback("v4-complete-missing-key").await;
    let draft = commit_cas(
        &harness,
        &operation_id(44),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "No Key Draft",
                "endpointUrl": "https://nokey.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "targets": default_targets()
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &draft).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let connection_id = drafted["connectionId"].clone();
    let provider_id = control_plane_draft_ids(&harness)[0].clone();
    let complete = commit_cas(
        &harness,
        &operation_id(45),
        json!({
            "mode": "complete",
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "No Key Draft",
                    "endpointUrl": "https://nokey.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": default_targets()
        }),
    );
    let (status, error) = send_v4(&harness, Method::POST, "/onboarding/commit", &complete).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_eq!(error["code"], "invalidRequest");
    assert_eq!(dynamic_provider_count(&harness), 0);
    assert_eq!(account_count_for(&harness, &provider_id), 0);
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .provider_is_onboarding_draft(&provider_id)
            .unwrap(),
        Some(true)
    );
    assert!(!operation_exists(&harness, &operation_id(45)));
    harness.stop();
}

#[tokio::test]
async fn endpoint_edit_preserves_grants_until_explicit_authorize() {
    let harness = start_loopback("v4-draft-authorize").await;
    let secret = "sk-draft-grant";
    let first = commit_cas(
        &harness,
        &operation_id(46),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Grant Draft",
                "endpointUrl": "https://grant-a.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "authorization": api_key_auth(secret, None),
            "targets": default_targets()
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &first).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let connection_id = drafted["connectionId"].as_str().unwrap().to_string();
    let account_id = drafted["accountId"].as_str().unwrap().to_string();
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let original_binding = credential_of(&listed, "account", &account_id)["bindings"][0].clone();
    let refused = commit_cas(
        &harness,
        &operation_id(47),
        json!({
            "mode": "complete",
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Grant Draft",
                    "endpointUrl": "https://grant-b.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": default_targets()
        }),
    );
    let (status, error) = send_v4(&harness, Method::POST, "/onboarding/commit", &refused).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let preserved = &credential_of(&listed, "account", &account_id)["bindings"][0];
    assert_eq!(
        preserved["allowedEndpointIds"],
        original_binding["allowedEndpointIds"]
    );
    assert_eq!(
        preserved["allowedOrigins"],
        original_binding["allowedOrigins"]
    );
    let authorized = commit_cas(
        &harness,
        &operation_id(48),
        json!({
            "mode": "complete",
            "authorizeCurrentEndpoint": true,
            "connection": {
                "kind": "existing",
                "connectionId": connection_id,
                "configuration": {
                    "templateId": "custom-http",
                    "name": "Grant Draft",
                    "endpointUrl": "https://grant-b.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions",
                    "authKind": "bearer"
                }
            },
            "targets": default_targets()
        }),
    );
    let (status, completed) =
        send_v4(&harness, Method::POST, "/onboarding/commit", &authorized).await;
    assert_eq!(status, StatusCode::OK, "{completed}");
    let (status, listed) = send_v4(&harness, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let updated = &credential_of(&listed, "account", &account_id)["bindings"][0];
    let origins = updated["allowedOrigins"].as_array().unwrap();
    assert!(
        origins
            .iter()
            .any(|origin| origin.as_str().unwrap_or("").contains("grant-b.example")),
        "{updated}"
    );
    harness.stop();
}

#[tokio::test]
async fn v3_update_preserves_draft_flag() {
    let harness = start_loopback("v4-draft-v3-update").await;
    let body = commit_cas(
        &harness,
        &operation_id(49),
        json!({
            "mode": "draft",
            "connection": {
                "kind": "new",
                "templateId": "custom-http",
                "name": "Stay Draft",
                "endpointUrl": "https://stay-draft.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer"
            },
            "targets": default_targets()
        }),
    );
    let (status, drafted) = send_v4(&harness, Method::POST, "/onboarding/commit", &body).await;
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let provider_id = control_plane_draft_ids(&harness)[0].clone();
    let (status, updated) = send_v3(
        &harness,
        Method::PATCH,
        &format!("/providers/{provider_id}"),
        &cas(
            &harness,
            json!({
                "name": "Stay Draft Edited",
                "endpointUrl": "https://stay-draft.example/v1/chat/completions",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{
                    "publicModel": "lab-opus",
                    "upstreamModel": "vendor/opus"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(dynamic_provider_count(&harness), 0);
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .provider_is_onboarding_draft(&provider_id)
            .unwrap(),
        Some(true)
    );
    harness.stop();
}

#[tokio::test]
async fn cpa_catalog_selection_is_local_and_cas_protected() {
    let harness = start_loopback("v4-cpa-catalog").await;
    let (status, empty) = send_v4(&harness, Method::GET, "/cpa/models", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["models"], json!([]));

    let (status, rejected) = send_v4(
        &harness,
        Method::PUT,
        "/cpa/models",
        &cas(&harness, json!({ "enabledIds": ["gpt-5"] })),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{rejected}");

    harness
        .state
        .activate_cpa_model_catalog(
            vec![
                ocg_core::db::CpaCatalogModel {
                    id: "gpt-5".into(),
                    owned_by: Some("openai".into()),
                    enabled: true,
                },
                ocg_core::db::CpaCatalogModel {
                    id: "claude".into(),
                    owned_by: Some("anthropic".into()),
                    enabled: false,
                },
            ],
            "http://127.0.0.1:8317",
            chrono::Utc::now(),
        )
        .unwrap();

    let (status, listed) = send_v4(&harness, Method::GET, "/cpa/models", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["models"][0]["id"], "gpt-5");
    assert_eq!(listed["models"][0]["enabled"], true);
    assert_eq!(listed["models"][1]["id"], "claude");
    assert_eq!(listed["models"][1]["enabled"], false);
    assert_eq!(harness.state.cpa_model_catalog().as_ref(), &["gpt-5"]);

    let (status, unknown) = send_v4(
        &harness,
        Method::PUT,
        "/cpa/models",
        &cas(&harness, json!({ "enabledIds": ["missing"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");

    let (status, updated) = send_v4(
        &harness,
        Method::PUT,
        "/cpa/models",
        &cas(&harness, json!({ "enabledIds": ["claude"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["models"][0]["enabled"], false);
    assert_eq!(updated["models"][1]["enabled"], true);
    assert_eq!(harness.state.cpa_model_catalog().as_ref(), &["claude"]);
    harness.stop();
}

#[tokio::test]
async fn provider_catalog_remove_is_local_and_cas_protected() {
    use ocg_core::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope};

    let harness = start_loopback("v4-catalog-remove").await;
    let (status, missing) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["drop-me"] })),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{missing}");

    let now = Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(OPENCODE_PROVIDER_ID),
            &["keep-me".into(), "drop-me".into()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    let (status, rejected) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["unknown"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");

    let (status, custom) = send_v4(
        &harness,
        Method::POST,
        "/provider-contracts/custom_endpoint/acct-1/catalog/remove",
        &cas(&harness, json!({ "modelIds": ["drop-me"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{custom}");

    let before = harness.state.settings_revision();
    let (status, removed) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["drop-me"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["removedIds"], json!(["drop-me"]));
    assert_eq!(removed["catalogModels"], json!(["keep-me"]));
    assert_eq!(removed["revision"]["revision"], before + 1);
    let stored = harness
        .state
        .db
        .lock()
        .load_persisted_scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))
        .unwrap()
        .unwrap();
    assert_eq!(stored.catalog_models, vec!["keep-me"]);
    assert!(
        harness
            .state
            .provider_contracts()
            .scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))
            .is_some_and(|contract| {
                contract.model("keep-me").is_some() && contract.model("drop-me").is_none()
            })
    );
    harness.stop();
}

#[tokio::test]
async fn zen_catalog_remove_last_and_all_stay_empty_after_reload() {
    use ocg_core::kernel::zen::ZenFreeModelCatalog;
    use ocg_core::provider_contracts::ContractScope;

    let harness = start_loopback("v4-zen-catalog-remove-empty").await;
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_ZEN_FREE_PROVIDER_ID);
    harness
        .state
        .activate_zen_free_model_catalog(ZenFreeModelCatalog {
            models: vec!["review-model-free".into(), "second-free".into()],
            refreshed_at: Some(now),
            source_url: "https://example.test/zen".into(),
        })
        .unwrap();

    let (status, last) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["second-free"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{last}");
    assert_eq!(last["catalogModels"], json!(["review-model-free"]));

    let before = harness.state.settings_revision();
    let (status, removed) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["review-model-free"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["removedIds"], json!(["review-model-free"]));
    assert_eq!(removed["catalogModels"], json!([]));
    assert_eq!(removed["revision"]["revision"], before + 1);
    assert!(
        harness
            .state
            .provider_contracts()
            .scope(&scope)
            .is_some_and(|contract| {
                contract.catalog.models.is_empty()
                    && contract.model("review-model-free").is_none()
                    && !contract.model_has_enabled_protocol("review-model-free")
            })
    );

    harness.state.reload_provider_contracts().unwrap();
    assert!(
        harness
            .state
            .provider_contracts()
            .scope(&scope)
            .is_some_and(|contract| contract.catalog.models.is_empty())
    );

    let reopened = ocg_core::db::Database::open(harness.dir.clone()).unwrap();
    let snapshot = reopened.zen_free_model_catalog().unwrap().unwrap();
    assert_eq!(snapshot.models, vec!["review-model-free", "second-free"]);
    let restored = ocg_core::provider_contracts::build_effective_contracts(
        &snapshot,
        &[],
        reopened.load_persisted_contracts().unwrap(),
    );
    assert!(restored.scope(&scope).unwrap().catalog.models.is_empty());
    drop(reopened);

    harness
        .state
        .activate_zen_free_model_catalog(ZenFreeModelCatalog {
            models: vec!["again-free".into(), "more-free".into()],
            refreshed_at: Some(Utc::now()),
            source_url: "https://example.test/zen".into(),
        })
        .unwrap();
    let (status, cleared) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["again-free", "more-free"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["catalogModels"], json!([]));
    assert!(
        harness
            .state
            .provider_contracts()
            .scope(&scope)
            .is_some_and(|contract| contract.catalog.models.is_empty()
                && contract.model("again-free").is_none())
    );
    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catalog_remove_same_cas_allows_only_one_concurrent_success() {
    use ocg_core::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope};

    let harness = start_loopback("v4-catalog-remove-cas").await;
    let now = Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(OPENCODE_PROVIDER_ID),
            &["model-a".into(), "model-b".into()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let path = format!("/provider-contracts/provider/{OPENCODE_PROVIDER_ID}/catalog/remove");
    let body_a = json!({
        "expectedRevision": before,
        "processGeneration": generation,
        "modelIds": ["model-a"],
    });
    let body_b = json!({
        "expectedRevision": before,
        "processGeneration": generation,
        "modelIds": ["model-b"],
    });
    let first = send_v4(&harness, Method::POST, &path, &body_a);
    let second = send_v4(&harness, Method::POST, &path, &body_b);
    let (first, second) = tokio::join!(first, second);
    let statuses = [first.0, second.0];
    let ok = statuses
        .iter()
        .filter(|status| **status == StatusCode::OK)
        .count();
    let conflict = statuses
        .iter()
        .filter(|status| **status == StatusCode::CONFLICT)
        .count();
    assert_eq!(ok, 1, "{first:?} {second:?}");
    assert_eq!(conflict, 1, "{first:?} {second:?}");
    let loser = if first.0 == StatusCode::CONFLICT {
        &first.1
    } else {
        &second.1
    };
    assert_eq!(loser["code"], "revisionConflict");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let stored = harness
        .state
        .db
        .lock()
        .load_persisted_scope(&ContractScope::provider(OPENCODE_PROVIDER_ID))
        .unwrap()
        .unwrap();
    assert_eq!(stored.catalog_models.len(), 1);
    harness.stop();
}

#[tokio::test]
async fn catalog_remove_advances_revision_before_reload_failure() {
    use ocg_core::provider_contracts::{CATALOG_SOURCE_OPENCODE_MODELS, ContractScope};

    let harness = start_loopback("v4-catalog-remove-reload-fail").await;
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &["keep-me".into(), "drop-me".into()],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
    let before = harness.state.settings_revision();
    let before_contracts = harness.state.provider_contracts();

    let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER corrupt_catalog_remove_post_commit
         AFTER UPDATE ON provider_contract_scopes
         BEGIN
             INSERT INTO provider_contract_model_protocols
               (scope_kind, scope_id, model_id, protocol, source)
             VALUES (NEW.scope_kind, NEW.scope_id, 'corrupt', 'chat_completions', 'invalid-after-commit');
         END;",
    )
    .unwrap();

    let (status, body) = send_v4(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_PROVIDER_ID}/catalog/remove"),
        &cas(&harness, json!({ "modelIds": ["drop-me"] })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["code"], "internal");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let stored = harness
        .state
        .db
        .lock()
        .load_persisted_scope(&scope)
        .unwrap()
        .unwrap();
    assert_eq!(stored.catalog_models, vec!["keep-me"]);
    let after_contracts = harness.state.provider_contracts();
    let after_scope = after_contracts.scope(&scope).unwrap();
    assert_eq!(after_scope.catalog.models, vec!["keep-me"]);
    assert!(after_scope.model("drop-me").is_none());
    assert!(!after_scope.model_has_enabled_protocol("drop-me"));
    assert_eq!(
        after_scope.model("keep-me"),
        before_contracts.scope(&scope).unwrap().model("keep-me")
    );
    for (provider, before_scope) in &before_contracts.providers {
        if provider != scope.id() {
            assert_eq!(after_contracts.providers.get(provider), Some(before_scope));
        }
    }
    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn alias_publication_hides_public_name_from_v1_models_but_keeps_routing() {
    let secret = "sk-alias-pub";
    let mut replies = HashMap::new();
    replies.insert(
        secret.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_OK,
        }]),
    );
    let (upstream, calls, _stop) = start_fake_upstream(replies).await;
    let harness = start_loopback("v4-alias-publication").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            create_body(
                "Publication Lab",
                &format!("{upstream}/v1/chat/completions"),
                "chat_completions",
                "bearer",
                Some(secret),
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("provider id missing: {created}"))
        .to_string();
    let account_id = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .unwrap_or_else(|| panic!("missing account after create: {created}"));
    harness.enable_account(&account_id);

    let listed = listed_gateway_model_ids(&harness).await;
    assert!(
        listed.iter().any(|id| id == "lab-opus"),
        "live public alias missing: {listed:?}"
    );

    let (status, publication) =
        send_v4(&harness, Method::GET, "/alias-publication", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{publication}");
    assert_eq!(publication["unpublished"], json!([]));

    let (status, hidden) = send_v4(
        &harness,
        Method::PATCH,
        "/alias-publication",
        &cas(
            &harness,
            json!({
                "publicModel": "Lab-Opus",
                "published": false
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hidden}");
    assert_eq!(hidden["unpublished"], json!(["lab-opus"]));
    let listed_hidden = listed_gateway_model_ids(&harness).await;
    assert!(
        !listed_hidden
            .iter()
            .any(|id| id.eq_ignore_ascii_case("lab-opus")),
        "hidden alias leaked into /v1/models: {listed_hidden:?}"
    );

    let (chat_status, chat_body) = chat_completion(&harness, "lab-opus").await;
    assert_eq!(chat_status, StatusCode::OK, "{chat_body}");
    assert_eq!(calls.lock().expect("fake call log").len(), 1);

    let (status, shown) = send_v4(
        &harness,
        Method::PATCH,
        "/alias-publication",
        &cas(
            &harness,
            json!({
                "publicModel": "lab-opus",
                "published": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["unpublished"], json!([]));
    let listed_again = listed_gateway_model_ids(&harness).await;
    assert!(
        listed_again.iter().any(|id| id == "lab-opus"),
        "restored alias missing: {listed_again:?}"
    );

    let (status, invalid) = send_v4(
        &harness,
        Method::PATCH,
        "/alias-publication",
        &cas(
            &harness,
            json!({
                "publicModel": "  ",
                "published": false
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    harness.stop();
}

#[tokio::test]
async fn dsh_application_is_explicitly_unsupported_without_a_desktop_host() {
    let harness = start_loopback("v4-dsh-headless").await;
    let (status, body) = send_v4(&harness, Method::GET, "/applications/dsh", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "unsupported_runtime");
    assert_eq!(body["installSupported"], false);
    assert_eq!(body["installed"], false);
    assert!(body["fingerprint"].is_null());
    assert_secret_free(&body, &[&harness.state.config().gateway_key]);
    harness.stop();
}

#[tokio::test]
async fn dsh_application_install_resolves_the_selected_key_only_inside_the_host() {
    use ocg_core::dsh_application::{
        DshApplicationHostRequest, DshApplicationInspection, DshApplicationPhase,
    };
    use ocg_core::gateway_keys::PRIMARY_KEY_ID;
    use std::sync::{Arc, Mutex};

    let harness = start_loopback("v4-dsh-install").await;
    let selected = harness.state.config().gateway_key;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_host = calls.clone();
    harness
        .state
        .set_dsh_application_host(Arc::new(move |request| match request {
            DshApplicationHostRequest::Inspect { gateway_v1_url } => {
                calls_for_host
                    .lock()
                    .unwrap()
                    .push(format!("inspect:{gateway_v1_url}"));
                Ok(DshApplicationInspection {
                    phase: DshApplicationPhase::Ready,
                    detected: true,
                    installed: false,
                    install_supported: true,
                    activation_required: false,
                    version: Some("0.1.5-rc.2".into()),
                    detail: Some("ready".into()),
                    target_paths: vec!["DSH web profile".into()],
                    fingerprint: Some("inspection-fingerprint".into()),
                })
            }
            DshApplicationHostRequest::Install {
                expected_fingerprint,
                gateway_v1_url,
                secret,
            } => {
                assert_eq!(expected_fingerprint, "inspection-fingerprint");
                calls_for_host.lock().unwrap().push(format!(
                    "install:{gateway_v1_url}:{}",
                    secret.expose_to_host()
                ));
                Ok(DshApplicationInspection {
                    phase: DshApplicationPhase::Installed,
                    detected: true,
                    installed: true,
                    install_supported: true,
                    activation_required: true,
                    version: Some("0.1.5-rc.2".into()),
                    detail: Some("installed".into()),
                    target_paths: vec!["DSH web profile".into()],
                    fingerprint: Some("installed-fingerprint".into()),
                })
            }
        }));

    let (status, inspected) =
        send_v4(&harness, Method::GET, "/applications/dsh", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!(inspected["status"], "ready");
    let request = cas(
        &harness,
        json!({
            "keyId": PRIMARY_KEY_ID,
            "expectedFingerprint": inspected["fingerprint"]
        }),
    );
    let (status, installed) = send_v4(&harness, Method::POST, "/applications/dsh", &request).await;
    assert_eq!(status, StatusCode::OK, "{installed}");
    assert_eq!(installed["status"], "installed");
    assert_eq!(installed["installed"], true);
    assert_eq!(installed["activationRequired"], true);
    assert_secret_free(&installed, &[&selected]);
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.ends_with(&selected)),
        "selected Key did not reach the private host seam"
    );
    harness.stop();
}
