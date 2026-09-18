//! Dashboard V3 local/Zen providers control plane: auth, catalog, contracts,
//! CAS, Zen refresh, and retired V2 paths. Protocol probes live in
//! `dashboard_v3_provider_probes.rs`.

#[cfg(debug_assertions)]
use axum::Router;
#[cfg(debug_assertions)]
use axum::extract::OriginalUri;
#[cfg(debug_assertions)]
use axum::http::HeaderMap;
#[cfg(debug_assertions)]
use axum::routing::get;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::ERROR_CONFLICT;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::set_zen_models_source_url_override_for_tests;
use ocg_core::dashboard_v3::{
    AccountUpstreamProtocol, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_OUTBOUND_FAILED,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, ProviderCatalog, ProviderContracts,
    ProviderModelCapability, ZenFreeModels, ZenFreeSettings,
};
use ocg_core::kernel::ids::is_free_model;
use ocg_core::kernel::zen::ZEN_MODELS_SOURCE_URL;
#[cfg(debug_assertions)]
use ocg_core::models::ProxyMode;
use ocg_core::provider::{
    BUILTIN_PROVIDERS, COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, KIMI_PROVIDER_ID,
    MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID, OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID,
    ZEN_FREE_ACCOUNT_ID,
};
use ocg_core::provider_contracts::{
    CATALOG_SOURCE_COMMAND_CODE_MODELS, CATALOG_SOURCE_KIMI_CN_MODELS,
    CATALOG_SOURCE_MINIMAX_CN_MODELS, CATALOG_SOURCE_OFFICIAL_ZEN, CATALOG_SOURCE_OPENCODE_MODELS,
    ContractScope,
};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
#[cfg(debug_assertions)]
use std::sync::{Arc, Mutex};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

fn persist_catalog(harness: &V3Harness, provider_id: &str, models: &[&str], source: &str) {
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(provider_id),
            &models
                .iter()
                .map(|model| (*model).to_string())
                .collect::<Vec<_>>(),
            Some(now),
            source,
            "https://example.test/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();
}

#[cfg(debug_assertions)]
struct ZenUrlOverride {
    process_generation: u64,
}

#[cfg(debug_assertions)]
impl Drop for ZenUrlOverride {
    fn drop(&mut self) {
        set_zen_models_source_url_override_for_tests(self.process_generation, None);
    }
}

#[cfg(debug_assertions)]
fn override_zen_url(harness: &V3Harness, url: &str) -> ZenUrlOverride {
    let process_generation = harness.state.process_generation();
    set_zen_models_source_url_override_for_tests(process_generation, Some(url.to_string()));
    ZenUrlOverride { process_generation }
}

#[cfg(debug_assertions)]
#[derive(Clone, Debug)]
struct CapturedZenCall {
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    x_goog_api_key: Option<String>,
}

#[cfg(debug_assertions)]
struct ZenOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedZenCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(debug_assertions)]
impl ZenOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[cfg(debug_assertions)]
async fn start_zen_origin(status: StatusCode, body: Value) -> ZenOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(get(move |uri: OriginalUri, headers: HeaderMap| {
        let calls = calls_for_handler.clone();
        let body = body.clone();
        async move {
            calls.lock().unwrap().push(CapturedZenCall {
                path: uri.0.path().to_string(),
                authorization: headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                x_api_key: headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
                x_goog_api_key: headers
                    .get("x-goog-api-key")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string),
            });
            (status, axum::Json(body))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
            .ok();
    });
    ZenOrigin {
        url: format!("http://{addr}/zen/v1/models"),
        calls,
        _stop: stop,
    }
}

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = match patch {
        Value::Object(map) => map,
        _ => Map::new(),
    };
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

async fn send_json(
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
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn send_raw(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &str,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn get_v3(harness: &V3Harness, path: &str) -> (StatusCode, Value) {
    harness
        .get_json(&format!("{}{path}", harness.v3_base))
        .await
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

fn assert_v3_error(body: &Value, code: &str) {
    assert_eq!(body["code"], code, "{body}");
    assert!(body.get("message").and_then(Value::as_str).is_some());
    assert!(body.as_object().unwrap().contains_key("currentRevision"));
    assert!(body.as_object().unwrap().contains_key("processGeneration"));
    assert!(body.get("current_revision").is_none());
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
                    | "primaryKey"
                    | "primary_key"
                    | "referralCode"
                    | "referral_code"
                    | "cipher"
                    | "apiKey"
                    | "api_key"
                    | "token"
                    | "secret"
            ),
            "provider payload leaked field {name}: {body}"
        );
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "provider payload leaked credential {secret}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "provider payload leaked credential {secret}: {body}"
            );
        }
    }
}

fn assert_revision_snapshot(body: &Value, harness: &V3Harness) {
    assert_eq!(
        body["revision"].as_u64(),
        Some(harness.state.settings_revision()),
        "{body}"
    );
    assert_eq!(
        body["processGeneration"].as_u64(),
        Some(harness.state.process_generation()),
        "{body}"
    );
    assert_eq!(
        body["pricingRevision"].as_str(),
        Some(harness.state.pricing_snapshot().revision.as_str()),
        "{body}"
    );
}

fn get_paths() -> [&'static str; 5] {
    [
        "/providers",
        "/providers/model-capabilities",
        "/providers/zen-free",
        "/providers/zen-free/models",
        "/provider-contracts",
    ]
}

fn mutation_routes() -> Vec<(Method, String, Value)> {
    vec![
        (
            Method::PATCH,
            "/providers/zen-free".into(),
            json!({ "enabled": false }),
        ),
        (
            Method::POST,
            "/providers/zen-free/models/refresh".into(),
            json!({}),
        ),
    ]
}

fn custom_create_body() -> Value {
    json!({
        "name": "Lan",
        "key": "custom-secret-key",
        "providerId": CUSTOM_PROVIDER_ID,
        "customConfig": {
            "endpointUrl": "https://api.example.com/v1/messages",
            "upstreamProtocol": "messages"
        },
        "modelCapabilities": [{
            "modelId": "org/model",
            "protocol": "messages"
        }]
    })
}

#[tokio::test]
async fn dashboard_v3_provider_routes_require_the_v3_session() {
    let harness = start_public("providers-auth").await;

    for path in get_paths() {
        let (status, body) = get_v3(&harness, path).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    for (method, path, extra) in mutation_routes() {
        let (status, body) =
            send_json(&harness, method.clone(), &path, &cas(&harness, extra)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    let v2 = harness
        .client
        .get(format!("{}/providers", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 must stay an empty 401, got {v2_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_provider_reads() {
    let harness = start_public("providers-cookie").await;
    let register = harness
        .client
        .post(format!("{}/auth/register", harness.v2_base))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
    let cookie = register
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let listed = harness
        .client
        .get(format!("{}/providers", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body: Value = listed.json().await.unwrap();
    let parsed: ProviderCatalog = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.entries.len(),
        BUILTIN_PROVIDERS
            .iter()
            .filter(|plan| !plan.product_surface.is_external_integration())
            .count()
    );
    assert_secret_free(&body, &[]);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_providers_catalog_covers_all_plan_facts_nulls_and_camel_case() {
    let harness = start_loopback("providers-catalog").await;
    let (status, body) = get_v3(&harness, "/providers").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &[]);
    assert_revision_snapshot(&body, &harness);
    assert!(body.get("modelCapabilities").is_none());
    assert!(body.get("entries").is_some());
    assert!(body.get("provider_id").is_none());

    let parsed: ProviderCatalog = serde_json::from_value(body.clone()).expect("ProviderCatalog");
    let provider_plans = BUILTIN_PROVIDERS
        .iter()
        .filter(|plan| !plan.product_surface.is_external_integration())
        .collect::<Vec<_>>();
    assert_eq!(parsed.entries.len(), provider_plans.len());

    for (plan, entry) in provider_plans.into_iter().zip(parsed.entries.iter()) {
        assert_eq!(entry.provider_id, plan.provider_id);
        assert_eq!(entry.display_name, plan.display_name);
        assert_eq!(entry.display_family, plan.display_family);
        assert_eq!(entry.routable, plan.routable);
        assert_eq!(entry.singleton, plan.singleton_account_id.is_some());
        assert_eq!(
            entry.creation_availability,
            plan.creation_availability.as_str()
        );
        assert_eq!(
            entry.creation_unavailable_reason.as_deref(),
            plan.creation_unavailable_reason
        );
        assert_eq!(entry.verification_policy, plan.verification_policy.as_str());
        assert_eq!(
            entry.verification_runtime_availability,
            plan.verification_runtime_availability
        );
        assert_eq!(entry.pricing_availability, plan.pricing_availability);
        assert_eq!(entry.usage_availability, plan.usage_availability);
        assert_eq!(entry.key_prefix.as_deref(), plan.key_prefix);
        if !plan.routable {
            assert!(
                entry.model_aliases.is_empty(),
                "{} / {} must keep empty aliases",
                plan.provider_id,
                plan.provider_id
            );
        }
    }

    let entries = body["entries"].as_array().unwrap();
    for entry in entries {
        for field in [
            "creationUnavailableReason",
            "keyPrefix",
            "providerId",
            "modelAliases",
        ] {
            assert!(
                entry.as_object().unwrap().contains_key(field),
                "missing {field} on {entry}"
            );
        }
        assert!(entry.get("provider_id").is_none(), "{entry}");
        assert!(
            entry.get("creation_unavailable_reason").is_none(),
            "{entry}"
        );
    }

    let goat = entries
        .iter()
        .find(|entry| entry["providerId"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    assert_eq!(goat["routable"], true);
    assert_eq!(goat["verificationRuntimeAvailability"], "not_applicable");
    let goat_aliases = goat["modelAliases"].as_array().expect("GOAT aliases");
    assert!(
        goat_aliases.is_empty(),
        "unfetched GOAT must not publish leftover catalog aliases: {goat_aliases:?}"
    );
    assert_eq!(goat["keyPrefix"], Value::Null);

    let custom = entries
        .iter()
        .find(|entry| entry["providerId"] == CUSTOM_PROVIDER_ID)
        .unwrap();
    assert_eq!(custom["routable"], true);
    assert_eq!(custom["pricingAvailability"], "unpriced");
    assert_eq!(custom["verificationRuntimeAvailability"], "available");

    let zen = entries
        .iter()
        .find(|entry| entry["providerId"] == OPENCODE_ZEN_FREE_PROVIDER_ID)
        .unwrap();
    assert_eq!(zen["routable"], true);
    assert_eq!(zen["singleton"], true);
    assert!(zen["creationUnavailableReason"].is_string());
    let aliases = zen["modelAliases"].as_array().unwrap();
    assert!(
        !aliases.iter().any(|alias| alias == "mimo-v2.5-free"),
        "{aliases:?}"
    );
    assert!(
        !aliases.iter().any(|alias| alias == "mimo-v2.5"),
        "unfetched Zen must not publish a leftover seed alias: {aliases:?}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_catalog_aliases_follow_saved_builtin_catalogs() {
    let harness = start_loopback("providers-catalog-cn-aliases").await;
    let now = chrono::Utc::now();
    let minimax = [
        "MiniMax-M3",
        "MiniMax-M2.7",
        "MiniMax-M2.7-highspeed",
        "MiniMax-M2.5",
        "MiniMax-M2.5-highspeed",
        "MiniMax-M2.1",
        "MiniMax-M2.1-highspeed",
        "MiniMax-M2",
    ]
    .map(str::to_string);
    let kimi = [
        "kimi-for-coding",
        "kimi-for-coding-highspeed",
        "k3",
        "k3-256k",
    ]
    .map(str::to_string);
    let goat = ["nvidia/nemotron-3-ultra-550b-a55b"].map(str::to_string);
    {
        let db = harness.state.db.lock();
        db.set_contract_catalog(
            &ContractScope::provider(COMMAND_CODE_PROVIDER_ID),
            &goat,
            Some(now),
            CATALOG_SOURCE_COMMAND_CODE_MODELS,
            "https://api.commandcode.ai/v1/models",
            now,
        )
        .unwrap();
        db.set_contract_catalog(
            &ContractScope::provider(MINIMAX_PROVIDER_ID),
            &minimax,
            Some(now),
            CATALOG_SOURCE_MINIMAX_CN_MODELS,
            "https://api.minimaxi.com/v1/models",
            now,
        )
        .unwrap();
        db.set_contract_catalog(
            &ContractScope::provider(KIMI_PROVIDER_ID),
            &kimi,
            Some(now),
            CATALOG_SOURCE_KIMI_CN_MODELS,
            "https://api.kimi.com/coding/v1/models",
            now,
        )
        .unwrap();
    }
    harness.state.reload_provider_contracts().unwrap();

    let (status, body) = get_v3(&harness, "/providers").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProviderCatalog = serde_json::from_value(body).expect("ProviderCatalog");
    let goat_entry = parsed
        .entries
        .iter()
        .find(|entry| entry.provider_id == COMMAND_CODE_PROVIDER_ID)
        .expect("Command Code catalog entry");
    assert_eq!(
        goat_entry.model_aliases,
        ["nemotron-3-ultra"],
        "GOAT should expose only code-owned short Aliases from its saved catalog"
    );
    let minimax_entry = parsed
        .entries
        .iter()
        .find(|entry| entry.provider_id == MINIMAX_PROVIDER_ID)
        .expect("MiniMax catalog entry");
    assert_eq!(
        minimax_entry.model_aliases,
        [
            "minimax-m2",
            "minimax-m2.1",
            "minimax-m2.1-highspeed",
            "minimax-m2.5",
            "minimax-m2.5-highspeed",
            "minimax-m2.7",
            "minimax-m2.7-highspeed",
            "minimax-m3",
        ]
    );
    let kimi_entry = parsed
        .entries
        .iter()
        .find(|entry| entry.provider_id == KIMI_PROVIDER_ID)
        .expect("Kimi catalog entry");
    assert_eq!(
        kimi_entry.model_aliases,
        [
            "kimi-k2.7-code",
            "kimi-k2.7-code-highspeed",
            "kimi-k3",
            "kimi-k3-256k",
        ]
    );

    let (status, contracts_body) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{contracts_body}");
    let contracts: ProviderContracts =
        serde_json::from_value(contracts_body).expect("ProviderContracts");
    let kimi_contract = contracts
        .providers
        .iter()
        .find(|group| group.provider_id == KIMI_PROVIDER_ID)
        .expect("Kimi provider contracts");
    let aliases = kimi_contract
        .models
        .iter()
        .map(|model| (model.model_id.as_str(), model.alias.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(aliases.get("kimi-for-coding"), Some(&"kimi-k2.7-code"));
    assert_eq!(
        aliases.get("kimi-for-coding-highspeed"),
        Some(&"kimi-k2.7-code-highspeed")
    );
    assert_eq!(aliases.get("k3"), Some(&"kimi-k3"));
    assert_eq!(aliases.get("k3-256k"), Some(&"kimi-k3-256k"));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_model_capabilities_are_go_protocol_rows_including_grok_45() {
    let harness = start_loopback("providers-capabilities").await;
    let (status, body) = get_v3(&harness, "/providers/model-capabilities").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.is_array(), "{body}");
    assert_secret_free(&body, &[]);
    let parsed: Vec<ProviderModelCapability> =
        serde_json::from_value(body.clone()).expect("capabilities");
    assert!(
        parsed
            .iter()
            .all(|row| row.provider_id == OPENCODE_PROVIDER_ID)
    );
    assert!(
        parsed
            .iter()
            .all(|row| row.provider_id == OPENCODE_PROVIDER_ID)
    );
    let grok = parsed
        .iter()
        .find(|row| row.model_id == "grok-4.5")
        .expect("grok-4.5");
    assert_eq!(grok.preferred_protocol, AccountUpstreamProtocol::Responses);
    assert_eq!(
        grok.supported_protocols,
        vec![AccountUpstreamProtocol::Responses]
    );
    assert_eq!(body[0].get("model_id"), None);
    assert!(body[0].get("modelId").is_some());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_contracts_project_builtin_scopes_and_custom_endpoints() {
    let harness = start_loopback("providers-contracts").await;
    persist_catalog(
        &harness,
        KIMI_PROVIDER_ID,
        &["kimi-for-coding", "kimi-k3"],
        CATALOG_SOURCE_KIMI_CN_MODELS,
    );
    let (status, body) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &[]);
    assert_revision_snapshot(&body, &harness);
    let parsed: ProviderContracts = serde_json::from_value(body.clone()).expect("contracts");
    assert_eq!(parsed.providers.len(), 6);
    let ids: Vec<_> = parsed
        .providers
        .iter()
        .map(|group| group.provider_id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            OPENCODE_PROVIDER_ID,
            OPENCODE_ZEN_FREE_PROVIDER_ID,
            COMMAND_CODE_PROVIDER_ID,
            MINIMAX_PROVIDER_ID,
            KIMI_PROVIDER_ID,
            OLLAMA_PROVIDER_ID,
        ]
    );
    assert!(parsed.custom_endpoints.is_empty());
    for group in &parsed.providers {
        if group.provider_id == OLLAMA_PROVIDER_ID {
            assert!(
                !group.card.protocol_probe,
                "Ollama Cloud is Chat-only and has no protocol-probe action"
            );
            assert!(group.card.catalog_refresh);
        } else {
            assert!(
                group.card.protocol_probe,
                "{} must expose the shared probe action",
                group.provider_id
            );
        }
    }
    assert!(body["providers"][0].get("scope_kind").is_none());
    assert_eq!(body["providers"][0]["scopeKind"], "provider");
    let kimi = parsed
        .providers
        .iter()
        .find(|group| group.provider_id == KIMI_PROVIDER_ID)
        .expect("Kimi provider contract");
    let coding = kimi
        .models
        .iter()
        .find(|model| model.model_id == "kimi-for-coding")
        .expect("Kimi coding fallback model");
    assert_eq!(coding.alias, "kimi-k2.7-code");
    assert!(
        kimi.models.iter().any(|model| model.model_id == "kimi-k3"),
        "Kimi fallback catalog must include kimi-k3"
    );

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, custom_create_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let custom_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, after) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_secret_free(&after, &["custom-secret-key"]);
    let projected: ProviderContracts = serde_json::from_value(after.clone()).unwrap();
    assert_eq!(projected.custom_endpoints.len(), 1);
    assert_eq!(projected.custom_endpoints[0].scope_id, custom_id);
    assert_eq!(
        projected.custom_endpoints[0].provider_id,
        CUSTOM_PROVIDER_ID
    );
    assert_eq!(projected.custom_endpoints[0].account.id, custom_id);
    assert_eq!(projected.custom_endpoints[0].account.name, "Lan");
    assert_eq!(after["customEndpoints"][0]["scopeKind"], "custom_endpoint");
    assert!(after.get("custom_endpoints").is_none());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_contracts_hide_zen_free_models_from_go_scope() {
    let harness = start_loopback("providers-contracts-go-free-filter").await;
    // Simulate a persisted Go catalog row that still contains Zen Free ids
    // (written before the refresh write filter existed).
    let now = chrono::Utc::now();
    harness
        .state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(OPENCODE_PROVIDER_ID),
            &[
                "glm-5.3".to_string(),
                "hy3-free".to_string(),
                "deepseek-v4-flash-free".to_string(),
            ],
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            "http://127.0.0.1/provider/v1/models",
            now,
        )
        .unwrap();
    harness.state.reload_provider_contracts().unwrap();

    // The routing view is untouched: the effective contract set keeps the
    // persisted catalog verbatim, free ids included.
    let contracts = harness.state.provider_contracts();
    let go_scope = contracts
        .providers
        .get(OPENCODE_PROVIDER_ID)
        .expect("go scope");
    assert!(go_scope.catalog.models.iter().any(|id| id == "hy3-free"));
    assert!(go_scope.models.contains_key("hy3-free"));
    drop(contracts);

    let (status, body) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProviderContracts = serde_json::from_value(body.clone()).expect("contracts");
    let go = parsed
        .providers
        .iter()
        .find(|group| group.provider_id == OPENCODE_PROVIDER_ID)
        .expect("go group");
    assert!(go.catalog.models.contains(&"glm-5.3".to_string()));
    assert!(
        !go.catalog.models.iter().any(|id| is_free_model(id)),
        "{:?}",
        go.catalog.models
    );
    assert!(go.models.iter().any(|model| model.model_id == "glm-5.3"));
    assert!(
        !go.models.iter().any(|model| is_free_model(&model.model_id)),
        "{:?}",
        go.models
            .iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>()
    );

    persist_catalog(
        &harness,
        OPENCODE_ZEN_FREE_PROVIDER_ID,
        &["mimo-v2.5-free"],
        CATALOG_SOURCE_OFFICIAL_ZEN,
    );
    let (status, body) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProviderContracts = serde_json::from_value(body.clone()).expect("contracts");

    // Zen Free keeps listing its own refreshed free models.
    let zen = parsed
        .providers
        .iter()
        .find(|group| group.provider_id == OPENCODE_ZEN_FREE_PROVIDER_ID)
        .expect("zen group");
    assert!(zen.catalog.models.iter().any(|id| is_free_model(id)));
    assert!(
        zen.models
            .iter()
            .any(|model| is_free_model(&model.model_id))
    );

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_provider_gets_never_fetch_upstream() {
    let origin = start_zen_origin(
        StatusCode::OK,
        json!({ "data": [{ "id": "should-not-fetch-free" }] }),
    )
    .await;
    let harness = start_loopback("providers-get-local").await;
    let _guard = override_zen_url(&harness, &origin.url);
    let before = harness.state.zen_free_model_catalog();

    for path in get_paths() {
        let (status, body) = get_v3(&harness, path).await;
        assert_eq!(status, StatusCode::OK, "{path} {body}");
    }

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.zen_free_model_catalog().models, before.models);
    assert_eq!(harness.state.zen_free_model_catalog().refreshed_at, None);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_zen_settings_cas_bumps_without_account_or_secrets() {
    let harness = start_loopback("providers-zen-patch").await;
    let (status, before_body) = get_v3(&harness, "/providers/zen-free").await;
    assert_eq!(status, StatusCode::OK, "{before_body}");
    let before: ZenFreeSettings = serde_json::from_value(before_body.clone()).unwrap();
    assert_eq!(before.account_id, ZEN_FREE_ACCOUNT_ID);
    assert!(before.enabled);
    assert!(before_body.get("account").is_none());
    assert_secret_free(&before_body, &[]);
    assert_revision_snapshot(&before_body, &harness);

    let revision = harness.state.settings_revision();
    let (status, missing) = send_raw(
        &harness,
        Method::PATCH,
        "/providers/zen-free",
        &json!({ "enabled": false, "processGeneration": harness.state.process_generation() })
            .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(harness.state.settings_revision(), revision);

    let (status, invalid) = send_raw(
        &harness,
        Method::PATCH,
        "/providers/zen-free",
        r#"{"expectedRevision":1,"enabled":false}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
    assert_v3_error(&invalid, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), revision);

    let (status, stale) = send_json(
        &harness,
        Method::PATCH,
        "/providers/zen-free",
        &json!({
            "enabled": false,
            "expectedRevision": revision.saturating_sub(1),
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), revision);

    let (status, patched) = send_json(
        &harness,
        Method::PATCH,
        "/providers/zen-free",
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_secret_free(&patched, &[]);
    assert!(patched.get("account").is_none());
    let parsed: ZenFreeSettings = serde_json::from_value(patched.clone()).unwrap();
    assert!(!parsed.enabled);
    assert_eq!(parsed.account_id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(parsed.revision, revision + 1);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    assert!(
        !harness
            .state
            .db
            .lock()
            .get_account(ZEN_FREE_ACCOUNT_ID)
            .unwrap()
            .unwrap()
            .enabled
    );

    let accounts = harness
        .client
        .get(format!("{}/account-records", harness.v3_base))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let zen = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["id"] == ZEN_FREE_ACCOUNT_ID)
        .unwrap();
    assert_eq!(zen["enabled"], false);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_zen_saved_models_are_the_persisted_snapshot() {
    let harness = start_loopback("providers-zen-models").await;
    let (status, body) = get_v3(&harness, "/providers/zen-free/models").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body, &[]);
    assert_revision_snapshot(&body, &harness);
    let parsed: ZenFreeModels = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.account_id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(parsed.source_url, ZEN_MODELS_SOURCE_URL);
    assert!(parsed.refreshed_at.is_none());
    assert!(
        parsed.models.is_empty(),
        "unfetched Zen must not keep leftover seed models: {:?}",
        parsed.models
    );
    assert_eq!(body["refreshedAt"], Value::Null);
    assert!(body.get("refreshed_at").is_none());

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn unified_zen_catalog_refresh_returns_the_shared_layout_with_new_models_off() {
    let origin = start_zen_origin(
        StatusCode::OK,
        json!({ "data": [{ "id": "unified-new-free" }] }),
    )
    .await;
    let harness = start_loopback("providers-unified-zen-refresh").await;
    let _guard = override_zen_url(&harness, &origin.url);
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();

    let (status, contracts) = send_json(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{OPENCODE_ZEN_FREE_PROVIDER_ID}/catalog/refresh"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contracts}");
    assert_eq!(origin.call_count(), 1);
    let zen = contracts["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_ZEN_FREE_PROVIDER_ID)
        .expect("Zen Free provider contract");
    assert_eq!(zen["catalog"]["source"], CATALOG_SOURCE_OFFICIAL_ZEN);
    assert_eq!(zen["catalog"]["models"], json!(["unified-new-free"]));
    let model = zen["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "unified-new-free")
        .expect("new Zen model stays visible");
    assert_eq!(model["routable"], false);
    assert_eq!(
        model["protocols"]["chat_completions"]["override"],
        "force_off"
    );
    assert_eq!(model["protocols"]["chat_completions"]["enabled"], false);
    assert!(model["protocols"]["responses"].is_null());
    assert!(model["protocols"]["messages"].is_null());

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_zen_refresh_persists_on_success_and_preserves_state_on_failure_or_busy() {
    let success = start_zen_origin(
        StatusCode::OK,
        json!({
            "object": "list",
            "data": [
                { "id": "paid-model" },
                { "id": "refresh-test-free" }
            ]
        }),
    )
    .await;
    let harness = start_loopback("providers-zen-refresh").await;
    let _guard = override_zen_url(&harness, &success.url);
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let before_models = harness.state.zen_free_model_catalog();
    let before_revision = harness.state.settings_revision();

    let (status, missing) = send_raw(
        &harness,
        Method::POST,
        "/providers/zen-free/models/refresh",
        &json!({ "processGeneration": harness.state.process_generation() }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(success.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before_revision);

    let (status, refreshed) = send_json(
        &harness,
        Method::POST,
        "/providers/zen-free/models/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{refreshed}");
    assert_eq!(success.call_count(), 1);
    let captured = success.calls.lock().unwrap()[0].clone();
    assert_eq!(captured.path, "/zen/v1/models");
    assert!(captured.authorization.is_none());
    assert!(captured.x_api_key.is_none());
    assert!(captured.x_goog_api_key.is_none());
    let parsed: ZenFreeModels = serde_json::from_value(refreshed.clone()).unwrap();
    assert_eq!(
        parsed
            .models
            .iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>(),
        vec!["refresh-test-free"]
    );
    assert_eq!(
        parsed.models[0].alias, "refresh-test",
        "a refreshed Zen Free row publishes its suffix-stripped Alias"
    );
    assert_eq!(parsed.source_url, ZEN_MODELS_SOURCE_URL);
    assert!(parsed.refreshed_at.is_some());
    assert_eq!(parsed.revision, before_revision + 1);
    assert_eq!(harness.state.settings_revision(), before_revision + 1);
    assert_eq!(
        harness.state.zen_free_model_catalog().models,
        vec!["refresh-test-free".to_string()]
    );

    let (status, catalog) = get_v3(&harness, "/providers").await;
    assert_eq!(status, StatusCode::OK, "{catalog}");
    let zen = catalog["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["providerId"] == OPENCODE_ZEN_FREE_PROVIDER_ID)
        .unwrap();
    let aliases = zen["modelAliases"].as_array().unwrap();
    assert!(aliases.iter().any(|alias| alias == "refresh-test"));
    assert!(!aliases.iter().any(|alias| alias == "mimo-v2.5-free"));

    let empty = start_zen_origin(StatusCode::OK, json!({ "data": [{ "id": "paid-only" }] })).await;
    drop(_guard);
    let _empty_guard = override_zen_url(&harness, &empty.url);
    let after_success = harness.state.settings_revision();
    let (status, empty_body) = send_json(
        &harness,
        Method::POST,
        "/providers/zen-free/models/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{empty_body}");
    assert_v3_error(&empty_body, ERROR_OUTBOUND_FAILED);
    assert_eq!(empty_body["currentRevision"], after_success);
    assert_eq!(
        empty_body["processGeneration"],
        harness.state.process_generation()
    );
    assert_eq!(harness.state.settings_revision(), after_success);
    assert_eq!(
        harness.state.zen_free_model_catalog().models,
        vec!["refresh-test-free".to_string()]
    );

    let failed =
        start_zen_origin(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": "no" })).await;
    drop(_empty_guard);
    let _fail_guard = override_zen_url(&harness, &failed.url);
    let (status, failed_body) = send_json(
        &harness,
        Method::POST,
        "/providers/zen-free/models/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed_body}");
    assert_v3_error(&failed_body, ERROR_OUTBOUND_FAILED);
    assert_eq!(harness.state.settings_revision(), after_success);
    assert_eq!(
        harness.state.zen_free_model_catalog().models,
        vec!["refresh-test-free".to_string()]
    );
    let runtime_logs = harness.state.db.lock().list_gateway_logs(50).unwrap();
    assert!(runtime_logs.iter().any(|log| {
        log.message
            == format!(
                "event=provider_catalog_refresh_succeeded provider={OPENCODE_ZEN_FREE_PROVIDER_ID} model_count=1 revision={after_success}"
            )
    }));
    assert_eq!(
        runtime_logs
            .iter()
            .filter(|log| {
                log.message
                    == format!(
                        "event=provider_catalog_refresh_failed provider={OPENCODE_ZEN_FREE_PROVIDER_ID} stage=fetch"
                    )
            })
            .count(),
        2,
        "empty and HTTP-failed refreshes are both recorded"
    );
    assert!(
        runtime_logs
            .iter()
            .all(|log| !log.message.contains(&success.url)
                && !log.message.contains(&empty.url)
                && !log.message.contains(&failed.url))
    );

    let _busy = harness.state.zen_free_models_refresh.lock().await;
    let (status, busy) = send_json(
        &harness,
        Method::POST,
        "/providers/zen-free/models/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{busy}");
    assert_v3_error(&busy, ERROR_CONFLICT);
    assert_eq!(harness.state.settings_revision(), after_success);
    drop(_busy);

    assert_ne!(before_models.models, vec!["refresh-test-free".to_string()]);
    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_zen_refresh_source_overrides_do_not_cross_talk_across_harnesses() {
    let origin_a =
        start_zen_origin(StatusCode::OK, json!({ "data": [{ "id": "iso-a-free" }] })).await;
    let origin_b =
        start_zen_origin(StatusCode::OK, json!({ "data": [{ "id": "iso-b-free" }] })).await;
    let harness_a = start_loopback("providers-zen-iso-a").await;
    let harness_b = start_loopback("providers-zen-iso-b").await;
    assert_ne!(
        harness_a.state.process_generation(),
        harness_b.state.process_generation()
    );
    let _guard_a = override_zen_url(&harness_a, &origin_a.url);
    let _guard_b = override_zen_url(&harness_b, &origin_b.url);

    for harness in [&harness_a, &harness_b] {
        let mut config = harness.state.config();
        config.proxy_mode = ProxyMode::Direct;
        harness.state.set_config(config).unwrap();
    }

    let body_a = cas(&harness_a, json!({}));
    let body_b = cas(&harness_b, json!({}));
    let (result_a, result_b) = tokio::join!(
        send_json(
            &harness_a,
            Method::POST,
            "/providers/zen-free/models/refresh",
            &body_a,
        ),
        send_json(
            &harness_b,
            Method::POST,
            "/providers/zen-free/models/refresh",
            &body_b,
        ),
    );

    assert_eq!(result_a.0, StatusCode::OK, "{}", result_a.1);
    assert_eq!(result_b.0, StatusCode::OK, "{}", result_b.1);
    let parsed_a: ZenFreeModels = serde_json::from_value(result_a.1.clone()).unwrap();
    let parsed_b: ZenFreeModels = serde_json::from_value(result_b.1.clone()).unwrap();
    assert_eq!(
        parsed_a
            .models
            .iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>(),
        vec!["iso-a-free"]
    );
    assert_eq!(
        parsed_b
            .models
            .iter()
            .map(|model| model.model_id.as_str())
            .collect::<Vec<_>>(),
        vec!["iso-b-free"]
    );
    assert_eq!(parsed_a.source_url, ZEN_MODELS_SOURCE_URL);
    assert_eq!(parsed_b.source_url, ZEN_MODELS_SOURCE_URL);
    assert_eq!(origin_a.call_count(), 1);
    assert_eq!(origin_b.call_count(), 1);
    assert_eq!(
        harness_a.state.zen_free_model_catalog().models,
        vec!["iso-a-free".to_string()]
    );
    assert_eq!(
        harness_b.state.zen_free_model_catalog().models,
        vec!["iso-b-free".to_string()]
    );

    harness_a.stop();
    harness_b.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_model_protocol_overrides_enforces_cas_and_persists() {
    let harness = start_loopback("providers-overrides").await;
    persist_catalog(
        &harness,
        OPENCODE_PROVIDER_ID,
        &["glm-5.2", "grok-4.5"],
        CATALOG_SOURCE_OPENCODE_MODELS,
    );
    let (status, listed) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let before = harness.state.settings_revision();
    let go = listed["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["providerId"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    let scope_revision = go["revision"].as_u64().unwrap();

    let path = "/provider-contracts/provider/opencode/model-protocol-overrides";

    let (status, missing) = send_raw(
        &harness,
        Method::PUT,
        path,
        &json!({
            "overrides": [{ "modelId": "glm-5.2", "protocol": "messages", "state": "force_off" }],
            "processGeneration": harness.state.process_generation()
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, stale) = send_json(
        &harness,
        Method::PUT,
        path,
        &json!({
            "overrides": [{ "modelId": "glm-5.2", "protocol": "messages", "state": "force_off" }],
            "expectedRevision": 1,
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, empty) = send_json(
        &harness,
        Method::PUT,
        path,
        &cas(&harness, json!({ "overrides": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{empty}");
    assert_v3_error(&empty, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, unknown_protocol) = send_json(
        &harness,
        Method::PUT,
        path,
        &cas(
            &harness,
            json!({
                "overrides": [{ "modelId": "glm-5.2", "protocol": "gemini", "state": "force_off" }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown_protocol}");
    assert_v3_error(&unknown_protocol, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, unknown_scope) = send_json(
        &harness,
        Method::PUT,
        "/provider-contracts/provider/not-a-provider/model-protocol-overrides",
        &cas(&harness, json!({
            "overrides": [{ "modelId": "glm-5.2", "protocol": "chat_completions", "state": "force_off" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{unknown_scope}");
    assert_v3_error(&unknown_scope, ERROR_NOT_FOUND);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, switched) = send_json(
        &harness,
        Method::PUT,
        path,
        &cas(
            &harness,
            json!({
                "overrides": [
                    { "modelId": "glm-5.2", "protocol": "messages", "state": "force_off" },
                    { "modelId": "grok-4.5", "protocol": "chat_completions", "state": "force_on" }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_secret_free(&switched, &[]);
    assert_eq!(switched["revision"].as_u64(), Some(before + 1));
    assert_eq!(harness.state.settings_revision(), before + 1);
    let go_after = switched["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["providerId"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    assert_ne!(go_after["revision"].as_u64(), Some(scope_revision));
    let glm = go_after["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "glm-5.2")
        .unwrap();
    assert_eq!(glm["protocols"]["messages"]["override"], "force_off");
    assert_eq!(glm["protocols"]["messages"]["enabled"], false);
    let grok = go_after["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "grok-4.5")
        .unwrap();
    assert_eq!(
        grok["protocols"]["chat_completions"]["override"],
        "force_on"
    );

    harness
        .assert_v2_path_removed(Method::GET, "/provider-contracts", None)
        .await;
    let (status, v3_contracts) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{v3_contracts}");
    let v3_go = v3_contracts["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["providerId"] == OPENCODE_PROVIDER_ID)
        .unwrap();
    let glm = v3_go["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "glm-5.2")
        .unwrap();
    assert_eq!(glm["protocols"]["messages"]["override"], "force_off");

    let custom_path = format!(
        "/provider-contracts/custom_endpoint/{ZEN_FREE_ACCOUNT_ID}/model-protocol-overrides"
    );
    let v3_custom = harness
        .client
        .put(format!("{}{custom_path}", harness.v3_base))
        .json(&cas(&harness, json!({
            "overrides": [{ "modelId": "glm-5.2", "protocol": "chat_completions", "state": "force_off" }]
        })))
        .send()
        .await
        .unwrap();
    assert_eq!(v3_custom.status(), StatusCode::NOT_FOUND);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_custom_endpoint_model_protocol_overrides_enforce_cas_and_persist() {
    let harness = start_loopback("providers-custom-overrides").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, custom_create_body()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let custom_id = created["account"]["id"].as_str().unwrap().to_string();
    let path = format!("/provider-contracts/custom-endpoint/{custom_id}/model-protocol-overrides");
    let before = harness.state.settings_revision();

    let (status, stale) = send_json(
        &harness,
        Method::PUT,
        &path,
        &json!({
            "overrides": [{ "modelId": "org/model", "protocol": "messages", "state": "force_off" }],
            "expectedRevision": 1,
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, missing) = send_json(
        &harness,
        Method::PUT,
        "/provider-contracts/custom-endpoint/not-an-account/model-protocol-overrides",
        &cas(&harness, json!({
            "overrides": [{ "modelId": "org/model", "protocol": "messages", "state": "force_off" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_v3_error(&missing, ERROR_NOT_FOUND);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, undeclared) = send_json(
        &harness,
        Method::PUT,
        &path,
        &cas(&harness, json!({
            "overrides": [{ "modelId": "org/model", "protocol": "chat_completions", "state": "force_on" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{undeclared}");
    assert_v3_error(&undeclared, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, switched) = send_json(
        &harness,
        Method::PUT,
        &path,
        &cas(&harness, json!({
            "overrides": [{ "modelId": "org/model", "protocol": "messages", "state": "force_off" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let endpoint = switched["customEndpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|endpoint| endpoint["scopeId"] == custom_id)
        .unwrap();
    let local = endpoint["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "org/model")
        .unwrap();
    assert_eq!(local["protocols"]["messages"]["override"], "force_off");
    assert_eq!(local["protocols"]["messages"]["enabled"], false);

    let (status, after) = get_v3(&harness, "/provider-contracts").await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let endpoint = after["customEndpoints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|endpoint| endpoint["scopeId"] == custom_id)
        .unwrap();
    let local = endpoint["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "org/model")
        .unwrap();
    assert_eq!(
        local["protocols"]["messages"]["override"], "force_off",
        "custom endpoint override must persist across reload"
    );

    harness.stop();
}

#[tokio::test]
async fn retired_v2_provider_routes_stay_gone_and_v3_omits_legacy_aliases() {
    let harness = start_loopback("providers-v2-retired").await;

    harness
        .assert_v2_path_removed(Method::GET, "/providers", None)
        .await;

    let (status, v3_catalog) = get_v3(&harness, "/providers").await;
    assert_eq!(status, StatusCode::OK, "{v3_catalog}");
    assert!(v3_catalog.get("entries").is_some());
    assert_eq!(
        v3_catalog["entries"].as_array().unwrap().len(),
        BUILTIN_PROVIDERS
            .iter()
            .filter(|plan| !plan.product_surface.is_external_integration())
            .count()
    );
    assert!(v3_catalog["entries"][0].get("providerId").is_some());
    assert!(v3_catalog["entries"][0].get("provider_id").is_none());

    for alias in [
        "/providers/catalog",
        "/models/capabilities",
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}/provider-models"),
    ] {
        let response = harness
            .client
            .get(format!("{}{alias}", harness.v3_base))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "V3 must not mount V2 alias {alias}"
        );
    }

    harness
        .assert_v2_path_removed(Method::GET, "/providers/catalog", None)
        .await;
    harness
        .assert_v2_path_removed(Method::GET, "/providers/model-capabilities", None)
        .await;

    let before = harness.state.settings_revision();
    harness
        .assert_v2_path_removed(
            Method::PATCH,
            "/providers/zen-free",
            Some(json!({ "enabled": false, "expected_revision": before })),
        )
        .await;
    assert_eq!(harness.state.settings_revision(), before);

    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        "/providers/zen-free",
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(harness.state.settings_revision(), before + 1);

    let (status, v3_zen) = get_v3(&harness, "/providers/zen-free").await;
    assert_eq!(status, StatusCode::OK, "{v3_zen}");
    assert_eq!(v3_zen["enabled"], false);
    assert!(v3_zen.get("account").is_none());

    harness.stop();
}
