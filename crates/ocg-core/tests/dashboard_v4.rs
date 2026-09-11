//! Dashboard V4 connection projection and onboarding commit.

use chrono::{Duration, Utc};
use ocg_core::models::{Account, AccountSetupStep, AccountType};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    ZEN_FREE_ACCOUNT_ID, default_credential_kind, default_quota_scope,
};
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::anonymous_binding_id_for;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;

#[allow(dead_code)]
#[path = "fixtures/fake_upstream.rs"]
mod fake_upstream;
#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use fake_upstream::start_fake_upstream;
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
async fn connections_project_dynamic_provider_with_unverified_key_as_unknown_and_eligible() {
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
    assert_eq!(connection["eligibility"]["state"], "eligible");
    assert_eq!(connection["eligibility"]["reason"], "none");
    assert_eq!(connection["credentialCount"], 1);
    assert_eq!(connection["enabledCredentialCount"], 1);
    harness.stop();
}

#[tokio::test]
async fn connections_keep_two_custom_accounts_with_same_url_separate_and_ordered() {
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
    let response = harness
        .client
        .get(format!("{}/contract", v4_base(&harness)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "unauthorized");
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
    assert_eq!(dynamic_provider_count(&harness), 1);
    let provider_id = harness.state.dynamic_providers()[0].id.clone();
    assert_eq!(account_count_for(&harness, &provider_id), 1);
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
async fn commit_responses_and_operation_rows_are_secret_free() {
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
async fn identities_keep_platform_parent_and_linked_key_separate_with_declared_relation() {
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
async fn identities_omit_subscription_for_dynamic_accounts() {
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
async fn identities_are_secret_free() {
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
async fn identities_require_session() {
    let harness = start_public("v4-identities-session").await;
    let response = harness
        .client
        .get(format!("{}/accounts", v4_base(&harness)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "unauthorized");
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
