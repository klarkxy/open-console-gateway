//! Dashboard V3 local accounts slice: auth, CAS, secrecy, Plan gates, and retired V2 paths.

use chrono::Utc;
use ocg_core::browser::browser_profile_paths;
use ocg_core::dashboard_v3::{
    Account, AccountList, AccountMutation, AccountSetupStep, AccountType,
    AccountVerificationStatus, ERROR_CONFLICT, ERROR_INTERNAL, ERROR_INVALID_JSON,
    ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND,
    ERROR_PRECONDITION_FAILED, ERROR_REVISION_CONFLICT, ERROR_SERVICE_UNAVAILABLE,
    ERROR_UNAUTHORIZED,
};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

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
            ),
            "account payload leaked field {name}: {body}"
        );
        for secret in secrets {
            assert!(
                !name.contains(secret),
                "account payload leaked credential {secret} in field name {name}: {body}"
            );
        }
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "account payload leaked credential {secret}: {body}"
            );
        }
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "account payload leaked credential {secret} in encoded JSON: {body}"
        );
    }
}

fn profile_tombstones_exist(data_dir: &Path, account_id: &str) -> bool {
    browser_profile_paths(data_dir, account_id)
        .unwrap()
        .into_iter()
        .any(|path| {
            path.parent()
                .and_then(|parent| std::fs::read_dir(parent).ok())
                .is_some_and(|entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!(".ocg-profile-delete-{account_id}"))
                    })
                })
        })
        || {
            let journal = data_dir.join("browser-profile-operations");
            journal.is_dir()
                && std::fs::read_dir(&journal)
                    .map(|entries| entries.filter_map(Result::ok).next().is_some())
                    .unwrap_or(false)
        }
}

fn parse_account(body: &Value) -> Account {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("Account JSON: {body}"))
}

fn parse_list(body: &Value) -> AccountList {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("AccountList JSON: {body}"))
}

fn parse_mutation(body: &Value) -> AccountMutation {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("AccountMutation JSON: {body}"))
}

fn mutation_account(body: &Value) -> Account {
    let mutation = parse_mutation(body);
    mutation.account.expect("mutation should return an account")
}

fn custom_write() -> Value {
    json!({
        "endpointUrl": "https://api.example.com/v1/messages",
        "upstreamProtocol": "messages"
    })
}

fn custom_capability() -> Value {
    json!({
        "modelId": "org/model",
        "protocol": "messages"
    })
}

fn mutation_routes(id: &str) -> Vec<(Method, String, Value)> {
    vec![
        (
            Method::POST,
            "/accounts".into(),
            json!({ "name": "CAS", "key": "sk-cas" }),
        ),
        (
            Method::POST,
            "/accounts/managed".into(),
            json!({ "name": "draft" }),
        ),
        (Method::PATCH, format!("/accounts/{id}"), json!({})),
        (Method::DELETE, format!("/accounts/{id}"), json!({})),
        (
            Method::PUT,
            "/accounts/order".into(),
            json!({ "accountIds": [ZEN_FREE_ACCOUNT_ID] }),
        ),
        (Method::POST, format!("/accounts/{id}/toggle"), json!({})),
        (
            Method::PATCH,
            format!("/accounts/{id}/setup"),
            json!({ "setupStep": "google_account" }),
        ),
        (
            Method::POST,
            format!("/accounts/{id}/reset-cooldown"),
            json!({}),
        ),
        (
            Method::PUT,
            format!("/accounts/{id}/custom-config"),
            json!({
                "endpointUrl": "https://api.example.com/v1/messages",
                "upstreamProtocol": "messages",
                "modelCapabilities": [custom_capability()]
            }),
        ),
        (
            Method::PUT,
            format!("/accounts/{id}/model-capabilities"),
            json!({ "capabilities": [custom_capability()] }),
        ),
    ]
}

#[tokio::test]
async fn dashboard_v3_account_routes_require_the_v3_session() {
    let harness = start_public("accounts-auth").await;
    let (status, body) = harness
        .get_json(&format!("{}/accounts", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let (status, body) = harness
        .get_json(&format!("{}/accounts/missing", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    for (method, path, extra) in mutation_routes("missing") {
        let (status, body) =
            send_json(&harness, method.clone(), &path, &cas(&harness, extra)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    let v2 = harness
        .client
        .get(format!("{}/accounts", harness.v2_base))
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
async fn dashboard_v3_v2_login_cookie_authorizes_account_reads() {
    let harness = start_public("accounts-cookie").await;
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
        .get(format!("{}/accounts", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let body: Value = listed.json().await.unwrap();
    let parsed = parse_list(&body);
    assert_eq!(
        parsed.process_generation,
        harness.state.process_generation()
    );
    assert!(
        parsed
            .accounts
            .iter()
            .any(|account| account.id == ZEN_FREE_ACCOUNT_ID)
    );
    assert_secret_free(&body, &[]);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_account_mutations_require_cas_tokens() {
    let harness = start_loopback("accounts-missing-cas").await;
    let before = harness.state.settings_revision();
    let routes = mutation_routes(ZEN_FREE_ACCOUNT_ID);

    for (method, path, extra) in &routes {
        let mut payload = extra.clone();
        payload["processGeneration"] = json!(harness.state.process_generation());
        let (status, body) = send_raw(&harness, method.clone(), path, &payload.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
        assert_eq!(harness.state.settings_revision(), before);
    }

    for (method, path, extra) in &routes {
        let mut payload = extra.clone();
        payload["expectedRevision"] = json!(before);
        let (status, body) = send_raw(&harness, method.clone(), path, &payload.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_INVALID_JSON);
        assert_eq!(harness.state.settings_revision(), before);
    }

    for (method, path, _) in &routes {
        let (status, body) = send_raw(&harness, method.clone(), path, "not-json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_INVALID_JSON);
        assert_eq!(harness.state.settings_revision(), before);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_rejects_account_mutations() {
    let harness = start_loopback("accounts-stale-cas").await;
    let generation = harness.state.process_generation();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({ "name": "CAS target", "key": "sk-cas-target" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = mutation_account(&created).id;
    let current_revision = harness.state.settings_revision();
    let routes = mutation_routes(&account_id);

    for (method, path, extra) in &routes {
        let mut payload = extra.clone();
        payload["expectedRevision"] = json!(current_revision.saturating_sub(1));
        payload["processGeneration"] = json!(generation);
        let (status, body) = send_json(&harness, method.clone(), path, &payload).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_REVISION_CONFLICT);
        assert_eq!(body["currentRevision"], current_revision);
        assert_eq!(body["processGeneration"], generation);
        assert_eq!(harness.state.settings_revision(), current_revision);
    }

    for (method, path, extra) in &routes {
        let mut payload = extra.clone();
        payload["expectedRevision"] = json!(current_revision);
        payload["processGeneration"] = json!(generation ^ 1);
        let (status, body) = send_json(&harness, method.clone(), path, &payload).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_REVISION_CONFLICT);
        assert_eq!(harness.state.process_generation(), generation);
        assert_eq!(harness.state.settings_revision(), current_revision);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_list_and_detail_are_secret_free() {
    const SECRET: &str = "opaque/account+key=42";
    let harness = start_loopback("accounts-list").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "listed",
                "key": SECRET,
                "password": "pw-secret",
                "referralCode": "ref-secret",
                "username": "user",
                "notes": "keep this"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_secret_free(&created, &[SECRET, "pw-secret", "ref-secret"]);
    let created_account = mutation_account(&created);
    assert_eq!(created_account.username.as_deref(), Some("user"));
    assert_eq!(created_account.notes.as_deref(), Some("keep this"));
    assert!(created_account.enabled);
    assert_eq!(
        created_account.verification_status,
        AccountVerificationStatus::NotRequired
    );
    assert!(created_account.plan_routable);

    let (status, listed) = harness
        .get_json(&format!("{}/accounts", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_secret_free(&listed, &[SECRET, "pw-secret", "ref-secret"]);
    let listed = parse_list(&listed);
    let found = listed
        .accounts
        .iter()
        .find(|account| account.id == created_account.id)
        .expect("created account");
    assert_eq!(found.name, "listed");
    assert_eq!(found.username.as_deref(), Some("user"));
    assert_eq!(listed.revision, harness.state.settings_revision());

    let (status, detail) = harness
        .get_json(&format!(
            "{}/accounts/{}",
            harness.v3_base, created_account.id
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_secret_free(&detail, &[SECRET, "pw-secret", "ref-secret"]);
    let detail = parse_account(&detail);
    assert_eq!(detail.id, created_account.id);
    assert_eq!(detail.custom_config, None);
    assert!(detail.notes.is_some());

    let stored = harness
        .state
        .db
        .lock()
        .get_account(&created_account.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.referral_code.as_deref(), Some("ref-secret"));
    assert_ne!(stored.key_cipher, SECRET);
    assert!(!stored.key_cipher.is_empty());

    harness
        .state
        .db
        .lock()
        .set_account_cooldown(
            &created_account.id,
            Some(Utc::now() + chrono::Duration::hours(1)),
            Some(&format!("legacy rate limit echoed {SECRET}")),
        )
        .unwrap();
    harness
        .state
        .db
        .lock()
        .set_account_auth_error(
            &created_account.id,
            Some(&format!("legacy auth failure echoed {SECRET}")),
        )
        .unwrap();
    harness
        .state
        .db
        .lock()
        .set_account_verification(
            &created_account.id,
            ocg_core::provider::ConnectionVerificationStatus::Failed,
            None,
            Some(&format!("connection verify echoed {SECRET}")),
        )
        .unwrap();

    let (status, listed) = harness
        .get_json(&format!("{}/accounts", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_secret_free(&listed, &[SECRET, "pw-secret", "ref-secret"]);
    let listed_account = parse_list(&listed)
        .accounts
        .into_iter()
        .find(|account| account.id == created_account.id)
        .expect("created account");
    assert!(
        listed_account
            .last_error
            .as_deref()
            .unwrap()
            .contains("legacy rate limit echoed")
    );
    assert!(
        listed_account
            .auth_error
            .as_deref()
            .unwrap()
            .contains("legacy auth failure echoed")
    );
    assert!(
        listed_account
            .verification_error
            .as_deref()
            .unwrap()
            .contains("connection verify echoed")
    );

    let (status, detail) = harness
        .get_json(&format!(
            "{}/accounts/{}",
            harness.v3_base, created_account.id
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_secret_free(&detail, &[SECRET, "pw-secret", "ref-secret"]);
    let detail_account = parse_account(&detail);
    assert!(
        detail_account
            .last_error
            .as_deref()
            .unwrap()
            .contains("legacy rate limit echoed")
    );
    assert!(
        detail_account
            .auth_error
            .as_deref()
            .unwrap()
            .contains("legacy auth failure echoed")
    );
    assert!(
        detail_account
            .verification_error
            .as_deref()
            .unwrap()
            .contains("connection verify echoed")
    );

    let (status, missing) = harness
        .get_json(&format!("{}/accounts/does-not-exist", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_v3_error(&missing, ERROR_NOT_FOUND);
    assert_secret_free(&missing, &[SECRET, "pw-secret", "ref-secret"]);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_create_gates_for_go_custom_goat_and_zen() {
    let harness = start_loopback("accounts-create-gates").await;
    let before = harness.state.settings_revision();

    let (status, go) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "Go", "key": "sk-go" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go}");
    let go = mutation_account(&go);
    assert!(go.enabled);
    assert_eq!(go.provider_id, OPENCODE_PROVIDER_ID);
    assert_eq!(
        go.verification_status,
        AccountVerificationStatus::NotRequired
    );
    assert!(go.plan_routable);
    assert_eq!(harness.state.settings_revision(), before + 1);

    let (status, goat) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "GOAT",
                "key": "goat-key",
                "providerId": COMMAND_CODE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{goat}");
    let goat = mutation_account(&goat);
    assert!(goat.enabled);
    assert_eq!(
        goat.verification_status,
        AccountVerificationStatus::NotRequired
    );
    assert!(goat.plan_routable);

    let (status, custom_err) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{custom_err}");

    let before_custom = harness.state.settings_revision();
    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": "http://127.0.0.1:1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "modelId": "dash-custom-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom = mutation_account(&custom);
    assert!(custom.enabled);
    assert_eq!(
        custom.verification_status,
        AccountVerificationStatus::Pending
    );
    assert!(custom.plan_routable);
    assert_eq!(
        custom.custom_config.as_ref().unwrap().endpoint_url,
        "http://127.0.0.1:1/chat/completions"
    );
    assert_eq!(
        custom.model_capabilities[0].public_model,
        "dash-custom-model"
    );
    assert_eq!(
        custom.model_capabilities[0].upstream_model,
        "dash-custom-model"
    );
    assert_eq!(harness.state.settings_revision(), before_custom + 1);

    let (status, zen) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Zen",
                "key": "unused",
                "providerId": OPENCODE_ZEN_FREE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{zen}");
    assert_v3_error(&zen, ERROR_INVALID_REQUEST);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_update_toggle_reorder_setup_and_cooldown() {
    let harness = start_loopback("accounts-lifecycle").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "Go", "key": "sk-go" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let go_id = mutation_account(&created).id;
    harness.enable_account(&go_id);
    let after_create = harness.state.settings_revision();

    let (status, renamed) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}"),
        &cas(&harness, json!({ "name": "Go renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(mutation_account(&renamed).name, "Go renamed");
    assert_eq!(harness.state.settings_revision(), after_create + 1);

    let after_rename = harness.state.settings_revision();
    let (status, noop) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}"),
        &cas(&harness, json!({ "name": "Go renamed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{noop}");
    assert_eq!(
        harness.state.settings_revision(),
        after_rename + 1,
        "V2 account patches bump even when the visible name is unchanged"
    );

    let (status, toggled) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{go_id}/toggle"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{toggled}");
    assert!(!mutation_account(&toggled).enabled);

    let (status, restored) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{go_id}/toggle"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    assert!(mutation_account(&restored).enabled);

    let (status, listed) = harness
        .get_json(&format!("{}/accounts", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut ids = parse_list(&listed)
        .accounts
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    ids.reverse();
    let (status, reordered) = send_json(
        &harness,
        Method::PUT,
        "/accounts/order",
        &cas(&harness, json!({ "accountIds": ids })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reordered}");
    let reordered = parse_list(&reordered);
    assert_eq!(reordered.accounts[0].id, go_id);

    let (status, cooldown) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{go_id}/reset-cooldown"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cooldown}");
    assert!(mutation_account(&cooldown).cooldown_until.is_none());

    let (status, zen_toggle) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}/toggle"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{zen_toggle}");
    assert_v3_error(&zen_toggle, ERROR_INVALID_REQUEST);

    let (status, zen_delete) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{zen_delete}");

    let (status, managed) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "  draft  " })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{managed}");
    let managed = mutation_account(&managed);
    assert_eq!(managed.account_type, AccountType::Managed);
    assert_eq!(managed.setup_step, AccountSetupStep::GoogleAccount);
    assert!(!managed.enabled);
    let setup_before = harness.state.settings_revision();
    let (status, same_step) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{}/setup", managed.id),
        &cas(&harness, json!({ "setupStep": "google_account" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{same_step}");
    assert_eq!(harness.state.settings_revision(), setup_before);

    let (status, skipped) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{}/setup", managed.id),
        &cas(&harness, json!({ "setupStep": "payment" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{skipped}");
    assert_v3_error(&skipped, ERROR_CONFLICT);

    let (status, ready) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{}/setup", managed.id),
        &cas(&harness, json!({ "setupStep": "ready" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{ready}");

    let (status, advanced) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{}/setup", managed.id),
        &cas(&harness, json!({ "setupStep": "opencode_registration" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{advanced}");
    assert_eq!(
        mutation_account(&advanced).setup_step,
        AccountSetupStep::OpencodeRegistration
    );
    assert_eq!(harness.state.settings_revision(), setup_before + 1);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_custom_invalidation_ack_and_enable_gates() {
    let harness = start_loopback("accounts-custom").await;
    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": custom_write(),
                "modelCapabilities": [custom_capability()]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = mutation_account(&custom).id;
    harness
        .state
        .db
        .lock()
        .set_account_verification(
            &custom_id,
            ocg_core::provider::ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            Some("previous"),
        )
        .unwrap();

    let (status, enable) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{custom_id}"),
        &cas(&harness, json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enable}");
    assert!(mutation_account(&enable).enabled);

    let (status, updated) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/custom-config"),
        &cas(
            &harness,
            json!({
                "endpointUrl": "https://api.example.net/v2/messages",
                "upstreamProtocol": "messages",
                "modelCapabilities": [custom_capability()]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let updated = mutation_account(&updated);
    assert!(
        updated.enabled,
        "config edits keep the account enabled: {updated:?}"
    );
    assert_eq!(
        updated.verification_status,
        AccountVerificationStatus::Pending
    );
    assert!(updated.connection_verified_at.is_none());
    assert!(updated.verification_error.is_none());

    // The protocol is editable after create; the config and capability rows
    // change in one CAS transaction,
    // advances CAS, and re-opens verification as pending while staying enabled.
    let before_protocol_change = harness.state.settings_revision();
    let (status, protocol) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/custom-config"),
        &cas(
            &harness,
            json!({
                "endpointUrl": "https://api.example.net/v2/chat/completions",
                "upstreamProtocol": "chat_completions",
                "modelCapabilities": [{
                    "modelId": "org/model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{protocol}");
    let protocol = mutation_account(&protocol);
    assert!(protocol.enabled);
    assert_eq!(
        protocol.verification_status,
        AccountVerificationStatus::Pending
    );
    assert_eq!(
        harness.state.settings_revision(),
        before_protocol_change + 1,
        "a committed protocol change must advance CAS"
    );

    harness
        .state
        .db
        .lock()
        .set_account_verification(
            &custom_id,
            ocg_core::provider::ConnectionVerificationStatus::Verified,
            Some(Utc::now()),
            None,
        )
        .unwrap();
    let before_capability_rejection = harness.state.settings_revision();
    let (status, rejected_caps) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/model-capabilities"),
        &cas(
            &harness,
            json!({
                "capabilities": [{
                    "modelId": "org/wrong-protocol",
                    "protocol": "messages"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected_caps}");
    assert_eq!(
        harness.state.settings_revision(),
        before_capability_rejection,
        "a pre-commit capability validation failure must not advance CAS"
    );

    let (status, caps) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/model-capabilities"),
        &cas(
            &harness,
            json!({
                "capabilities": [{
                    "modelId": "org/other",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{caps}");
    let caps = mutation_account(&caps);
    assert!(caps.enabled);
    assert_eq!(caps.verification_status, AccountVerificationStatus::Pending);
    assert_eq!(caps.model_capabilities[0].public_model, "org/other");

    // Custom verification is an optional tool: a pending Custom account may be
    // enabled explicitly without verifying first.
    let before_enable = harness.state.settings_revision();
    let (status, enable_pending) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{custom_id}"),
        &cas(&harness, json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enable_pending}");
    assert!(mutation_account(&enable_pending).enabled);
    assert_eq!(harness.state.settings_revision(), before_enable + 1);

    let (status, goat) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "GOAT",
                "key": "goat-key",
                "providerId": COMMAND_CODE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{goat}");
    let goat_id = mutation_account(&goat).id;
    harness.enable_account(&goat_id);
    let before_goat = harness.state.settings_revision();
    let (status, goat_disable) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{goat_id}/toggle"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{goat_disable}");
    let goat_disable = mutation_account(&goat_disable);
    assert!(!goat_disable.enabled);
    assert_eq!(
        goat_disable.verification_status,
        AccountVerificationStatus::NotRequired
    );
    assert_eq!(harness.state.settings_revision(), before_goat + 1);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_custom_config_and_capabilities_roll_back_together() {
    let harness = start_loopback("accounts-custom-atomic-rollback").await;
    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom post-read",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": custom_write(),
                "modelCapabilities": [custom_capability()]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = mutation_account(&custom).id;
    let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_custom_config_update
         BEFORE UPDATE OF endpoint_url ON account_custom_configs
         BEGIN
             SELECT RAISE(ABORT, 'forced atomic Custom update failure');
         END;",
    )
    .unwrap();

    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/custom-config"),
        &cas(
            &harness,
            json!({
                "endpointUrl": "https://committed.example.com/v2/messages",
                "upstreamProtocol": "messages",
                "modelCapabilities": [custom_capability()]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);
    let stored: (String, String) = conn
        .query_row(
            "SELECT endpoint_url, upstream_protocol FROM account_custom_configs WHERE account_id = ?1",
            [&custom_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored.0, "https://api.example.com/v1/messages");
    assert_eq!(stored.1, "messages");
    let capabilities = harness
        .state
        .db
        .lock()
        .list_account_model_capabilities(&custom_id)
        .unwrap();
    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].public_model, "org/model");
    assert_eq!(capabilities[0].protocol.as_str(), "messages");

    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_capabilities_commit_advances_revision_before_post_read_failure() {
    let harness = start_loopback("accounts-capabilities-post-read").await;
    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Capabilities post-read",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": custom_write(),
                "modelCapabilities": [custom_capability()]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = mutation_account(&custom).id;
    let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER corrupt_capability_post_read
         AFTER INSERT ON account_model_capabilities
         BEGIN
             UPDATE account_model_capabilities
                SET protocol = 'invalid-after-commit'
              WHERE account_id = NEW.account_id
                AND model_id = NEW.model_id
                AND protocol = NEW.protocol;
         END;",
    )
    .unwrap();

    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::PUT,
        &format!("/accounts/{custom_id}/model-capabilities"),
        &cas(
            &harness,
            json!({
                "capabilities": [{
                    "modelId": "org/committed",
                    "protocol": "messages"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(harness.state.settings_revision(), before + 1);
    let stored: (String, String) = conn
        .query_row(
            "SELECT model_id, protocol FROM account_model_capabilities WHERE account_id = ?1",
            [&custom_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored.0, "org/committed");
    assert_eq!(stored.1, "invalid-after-commit");

    drop(conn);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_delete_stages_and_restores_browser_profiles() {
    let harness = start_loopback("accounts-delete-profile").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "profile", "key": "sk-profile" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = mutation_account(&created).id;
    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"stale request must preserve this").unwrap();

    let stale_revision = harness.state.settings_revision().saturating_sub(1);
    let (status, stale) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &json!({
            "expectedRevision": stale_revision,
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert!(profile.join("Cookies").is_file());
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(&account_id)
            .unwrap()
            .is_some()
    );

    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let deleted = parse_mutation(&deleted);
    assert!(deleted.account.is_none());
    assert!(!profile.exists());
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(&account_id)
            .unwrap()
            .is_none()
    );

    let (status, missing) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_v3_error(&missing, ERROR_NOT_FOUND);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_delete_cas_precedes_stop_stage_and_resource_errors() {
    let harness = start_loopback("accounts-delete-cas-first").await;
    let stop_count = Arc::new(AtomicUsize::new(0));
    let counted = stop_count.clone();
    harness
        .state
        .browser
        .register_native_hooks(
            Arc::new(|_, _| Ok(())),
            Arc::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .unwrap();

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({ "name": "cas-first", "key": "sk-cas-first" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = mutation_account(&created).id;
    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("Cookies"),
        b"stale request must not stage this",
    )
    .unwrap();
    let current_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let stale = json!({
        "expectedRevision": current_revision.saturating_sub(1),
        "processGeneration": generation
    });

    let (status, existing) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &stale,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{existing}");
    assert_v3_error(&existing, ERROR_REVISION_CONFLICT);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);
    assert!(profile.join("Cookies").is_file());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(&account_id)
            .unwrap()
            .is_some()
    );

    let (status, zen) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}"),
        &stale,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{zen}");
    assert_v3_error(&zen, ERROR_REVISION_CONFLICT);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        ZEN_FREE_ACCOUNT_ID
    ));

    let (status, missing) =
        send_json(&harness, Method::DELETE, "/accounts/does-not-exist", &stale).await;
    assert_eq!(status, StatusCode::CONFLICT, "{missing}");
    assert_v3_error(&missing, ERROR_REVISION_CONFLICT);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);

    let live_delete = {
        let _browser_operation = harness.state.browser.operation().await;
        tokio::time::timeout(
            Duration::from_secs(3),
            send_json(
                &harness,
                Method::DELETE,
                &format!("/accounts/{account_id}"),
                &stale,
            ),
        )
        .await
        .expect("stale delete must not wait on a live browser operation")
    };
    assert_eq!(live_delete.0, StatusCode::CONFLICT, "{}", live_delete.1);
    assert_v3_error(&live_delete.1, ERROR_REVISION_CONFLICT);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);
    assert!(profile.join("Cookies").is_file());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));

    let (status, zen_valid) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{zen_valid}");
    assert_v3_error(&zen_valid, ERROR_INVALID_REQUEST);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);

    let (status, missing_valid) = send_json(
        &harness,
        Method::DELETE,
        "/accounts/does-not-exist",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing_valid}");
    assert_v3_error(&missing_valid, ERROR_NOT_FOUND);
    assert_eq!(stop_count.load(Ordering::SeqCst), 0);

    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_eq!(stop_count.load(Ordering::SeqCst), 1);
    assert!(!profile.exists());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_delete_maps_browser_stop_failure_to_service_unavailable() {
    let harness = start_loopback("accounts-delete-stop-503").await;
    harness
        .state
        .browser
        .register_native_hooks(
            Arc::new(|_, _| Ok(())),
            Arc::new(|_| Err(anyhow::anyhow!("injected browser stop failure"))),
        )
        .unwrap();

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({ "name": "stop-fail", "key": "sk-stop-fail" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account_id = mutation_account(&created).id;
    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"stop failure must preserve this").unwrap();
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_v3_error(&body, ERROR_SERVICE_UNAVAILABLE);
    assert_eq!(body["currentRevision"], before);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), before);
    assert!(
        harness
            .state
            .db
            .lock()
            .get_account(&account_id)
            .unwrap()
            .is_some()
    );
    assert!(profile.join("Cookies").is_file());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_managed_create_requires_invite_url() {
    let harness = start_loopback("accounts-managed-invite").await;
    let mut config = harness.state.config();
    config.opencode_invite_url.clear();
    harness.state.set_config(config).unwrap();
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "pending" })),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_v3_error(&body, ERROR_PRECONDITION_FAILED);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn retired_v2_account_mutations_do_not_create_or_toggle() {
    let harness = start_loopback("accounts-v2-retired").await;
    let v2_created = harness
        .client
        .post(format!("{}/accounts", harness.v2_base))
        .json(&json!({
            "name": "V2",
            "key": "sk-v2-secret",
            "expected_revision": harness.state.settings_revision()
        }))
        .send()
        .await
        .unwrap();
    V3Harness::assert_v2_removed(v2_created.status(), &v2_created.json().await.unwrap());

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "V3", "key": "sk-v3-secret" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_secret_free(&created, &["sk-v3-secret"]);
    let v3_id = mutation_account(&created).id;

    let v2_list = harness
        .client
        .get(format!("{}/accounts", harness.v2_base))
        .send()
        .await
        .unwrap();
    V3Harness::assert_v2_removed(v2_list.status(), &v2_list.json().await.unwrap());

    let v2_toggle = harness
        .client
        .post(format!("{}/accounts/{v3_id}/toggle", harness.v2_base))
        .send()
        .await
        .unwrap();
    V3Harness::assert_v2_removed(v2_toggle.status(), &v2_toggle.json().await.unwrap());
    let (status, detail) = harness
        .get_json(&format!("{}/accounts/{v3_id}", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(parse_account(&detail).enabled);

    let (status, toggled) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{v3_id}/toggle"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{toggled}");
    assert!(!mutation_account(&toggled).enabled);

    harness.stop();
}

async fn find_account_for_provider(harness: &V3Harness, provider_id: &str) -> Account {
    let (status, listed) = harness
        .get_json(&format!("{}/accounts", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    parse_list(&listed)
        .accounts
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .unwrap_or_else(|| panic!("missing account for {provider_id}"))
}

#[tokio::test]
async fn dynamic_accounts_are_plan_routable_and_stale_uuid_accounts_are_not() {
    let harness = start_loopback("accounts-dyn-routable").await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        &cas(
            &harness,
            json!({
                "name": "Lab",
                "endpointUrl": "http://127.0.0.1:9",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab",
                "models": [{"publicModel": "lab-opus", "upstreamModel": "vendor/opus"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let account = find_account_for_provider(&harness, &provider_id).await;
    assert!(account.plan_routable);
    assert!(account.enabled);

    let (status, detail) = harness
        .get_json(&format!("{}/accounts/{}", harness.v3_base, account.id))
        .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert!(parse_account(&detail).plan_routable);

    let (status, created_stale) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "Stale", "key": "sk-stale" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created_stale}");
    let stale_id = mutation_account(&created_stale).id;
    let stale_uuid = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    {
        let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
        conn.execute(
            "UPDATE accounts SET provider_id = ?1 WHERE id = ?2",
            [stale_uuid, stale_id.as_str()],
        )
        .unwrap();
    }
    let (status, stale) = harness
        .get_json(&format!("{}/accounts/{stale_id}", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{stale}");
    assert!(
        !parse_account(&stale).plan_routable,
        "a dangling UUID account stays unroutable while a current dynamic snapshot is present"
    );
    let (status, still_dynamic) = harness
        .get_json(&format!("{}/accounts/{}", harness.v3_base, account.id))
        .await;
    assert_eq!(status, StatusCode::OK, "{still_dynamic}");
    assert!(parse_account(&still_dynamic).plan_routable);
    harness.stop();
}
