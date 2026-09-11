//! Dashboard V4 read-only connection projection.

use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID,
};
use ocg_domain::connection::{LegacyConnectionKind, connection_id_for_legacy};
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
