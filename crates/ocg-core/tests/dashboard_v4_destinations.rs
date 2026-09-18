//! Dashboard V4 destination and credential listings (RFC stage 4b).

use ocg_core::dashboard_v4::{DestinationCredentialDto, DestinationDto};
use ocg_core::destination_projection::{DestinationProjection, load_persisted, project};
use ocg_core::models::{Account, AccountSetupStep, AccountType};
use ocg_core::provider::{
    CPA_ACCOUNT_ID, CPA_ACCOUNT_NAME, CPA_PROVIDER_ID, CUSTOM_PROVIDER_ID, CredentialKind,
    OPENCODE_PROVIDER_ID, QuotaScope,
};
use ocg_domain::credential::credential_id_for_legacy_account;
use ocg_domain::destination::{
    destination_id_for_builtin, destination_id_for_custom_account,
    destination_id_for_platform_account,
};
use ocg_domain::ids::{OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;

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
    harness.v4_base.clone()
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

async fn send_v4_text(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, String) {
    let response = harness
        .client
        .request(method, format!("{}{path}", v4_base(harness)))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    (status, text)
}

async fn send_v4(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value, String) {
    let (status, text) = send_v4_text(harness, method, path, body).await;
    let parsed = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, parsed, text)
}

fn destinations_of(body: &Value) -> &[Value] {
    body["destinations"].as_array().expect("destinations array")
}

fn credentials_of(body: &Value) -> &[Value] {
    body["credentials"].as_array().expect("credentials array")
}

fn find_destination<'a>(body: &'a Value, id: &str) -> &'a Value {
    destinations_of(body)
        .iter()
        .find(|destination| destination["id"] == id)
        .unwrap_or_else(|| panic!("missing destination {id} in {body}"))
}

fn live_and_persisted(harness: &V3Harness) -> (DestinationProjection, DestinationProjection) {
    let db = harness.state.db.lock();
    let live = match project(&db).expect("live project should read") {
        Ok(projection) => projection,
        Err(refusals) => panic!("live project refused: {refusals:?}"),
    };
    let stored = load_persisted(&db).expect("v50 shadow should load");
    (live, stored)
}

fn assert_v4_lists_match_projection(
    destinations: &Value,
    credentials: &Value,
    projection: &DestinationProjection,
) {
    let expected_destinations: Vec<DestinationDto> = projection
        .destinations
        .iter()
        .map(DestinationDto::from)
        .collect();
    let expected_credentials: Vec<DestinationCredentialDto> = projection
        .credentials
        .iter()
        .map(DestinationCredentialDto::from)
        .collect();
    assert_eq!(
        destinations["destinations"],
        serde_json::to_value(expected_destinations).expect("destination dtos should serialize"),
        "{destinations}"
    );
    assert_eq!(
        credentials["credentials"],
        serde_json::to_value(expected_credentials).expect("credential dtos should serialize"),
        "{credentials}"
    );
}

fn find_credential<'a>(body: &'a Value, id: &str) -> &'a Value {
    credentials_of(body)
        .iter()
        .find(|credential| credential["id"] == id)
        .unwrap_or_else(|| panic!("missing credential {id} in {body}"))
}

#[tokio::test]
async fn fresh_state_lists_only_zen_and_matches_contract_revision() {
    let harness = start_loopback("v4-dest-fresh").await;
    let (status, contract, _) = send_v4(&harness, Method::GET, "/contract", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{contract}");

    let (status, destinations, _) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, _) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");

    assert_eq!(destinations["revision"], contract);
    assert_eq!(credentials["revision"], contract);
    let listed = destinations_of(&destinations);
    assert_eq!(listed.len(), 1, "{destinations}");
    let zen_id = destination_id_for_builtin(OPENCODE_ZEN_FREE_PROVIDER_ID);
    assert_eq!(listed[0]["id"], zen_id);
    assert_eq!(listed[0]["adapter"], "zen");
    assert_eq!(listed[0]["authScheme"], "none");
    assert_eq!(listed[0]["maxCredentials"], 1);

    let listed_credentials = credentials_of(&credentials);
    assert_eq!(listed_credentials.len(), 1, "{credentials}");
    let zen_credential_id = credential_id_for_legacy_account(ZEN_FREE_ACCOUNT_ID);
    assert_eq!(listed_credentials[0]["id"], zen_credential_id.as_str());
    assert_eq!(listed_credentials[0]["destinationId"], zen_id);
    assert_eq!(listed_credentials[0]["hasSecret"], false);
    harness.stop();
}

#[tokio::test]
async fn populated_state_projects_go_custom_and_platform_without_secrets() {
    let harness = start_loopback("v4-dest-populated").await;
    const GO_KEY: &str = "sk-dest-go-must-not-leak";
    const CUSTOM_KEY: &str = "sk-dest-custom-must-not-leak";
    const LINKED_KEY: &str = "sk-dest-linked-must-not-leak";
    const PLATFORM_SECRET: &str = "sk-dest-platform-observer-must-not-leak";

    let (status, go) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": "Go Key",
                "key": GO_KEY
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go}");
    let go_account_id = go["account"]["id"].as_str().unwrap().to_string();

    let (status, custom) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Custom Endpoint",
                "key": CUSTOM_KEY,
                "customConfig": {
                    "endpointUrl": "https://custom-dest.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "custom-model",
                    "upstreamModel": "custom-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_account_id = custom["account"]["id"].as_str().unwrap().to_string();

    let (status, parent) = send_v3(
        &harness,
        Method::POST,
        "/platform-accounts",
        &cas(
            &harness,
            json!({
                "kind": "new_api",
                "name": "Parent",
                "baseUrl": "https://platform-dest.example/v1",
                "userCredential": PLATFORM_SECRET
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{parent}");
    let parent_id = parent["accounts"][0]["id"].as_str().unwrap().to_string();

    let (status, linked) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Linked Custom",
                "key": LINKED_KEY,
                "customConfig": {
                    "endpointUrl": "https://old-dest.example/v1/chat/completions",
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
    assert_eq!(status, StatusCode::OK, "{linked}");
    let linked_account_id = linked["account"]["id"].as_str().unwrap().to_string();
    let (status, attached) = send_v3(
        &harness,
        Method::PUT,
        &format!("/accounts/{linked_account_id}/platform-link"),
        &cas(
            &harness,
            json!({
                "platformAccountId": parent_id,
                "group": { "id": "default", "autoGroups": [], "verified": false }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{attached}");

    let (status, destinations, destinations_raw) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, credentials_raw) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");

    for secret in [GO_KEY, CUSTOM_KEY, LINKED_KEY, PLATFORM_SECRET] {
        assert!(
            !destinations_raw.contains(secret),
            "secret leaked in destinations: {destinations_raw}"
        );
        assert!(
            !credentials_raw.contains(secret),
            "secret leaked in credentials: {credentials_raw}"
        );
    }
    for blob in [&destinations_raw, &credentials_raw] {
        assert!(
            !blob.contains("key_cipher"),
            "V4 GET must not name key_cipher: {blob}"
        );
        assert!(
            !blob.contains("credential_cipher"),
            "V4 GET must not name credential_cipher: {blob}"
        );
    }

    let go_destination_id = destination_id_for_builtin(OPENCODE_PROVIDER_ID);
    let custom_destination_id = destination_id_for_custom_account(&custom_account_id);
    let platform_destination_id = destination_id_for_platform_account(&parent_id);
    let go_destination = find_destination(&destinations, &go_destination_id);
    let custom_destination = find_destination(&destinations, &custom_destination_id);
    let platform_destination = find_destination(&destinations, &platform_destination_id);
    assert_eq!(go_destination["adapter"], "opencode_go");
    assert_eq!(custom_destination["adapter"], "http");
    assert_eq!(custom_destination["maxCredentials"], 1);
    assert_eq!(platform_destination["adapter"], "http");
    assert_eq!(platform_destination["capabilities"]["observer"], true);

    let go_credential = find_credential(
        &credentials,
        credential_id_for_legacy_account(&go_account_id).as_str(),
    );
    let custom_credential = find_credential(
        &credentials,
        credential_id_for_legacy_account(&custom_account_id).as_str(),
    );
    let linked_credential = find_credential(
        &credentials,
        credential_id_for_legacy_account(&linked_account_id).as_str(),
    );
    let zen_credential = find_credential(
        &credentials,
        credential_id_for_legacy_account(ZEN_FREE_ACCOUNT_ID).as_str(),
    );
    assert_eq!(go_credential["destinationId"], go_destination_id);
    assert_eq!(custom_credential["destinationId"], custom_destination_id);
    assert_eq!(linked_credential["destinationId"], platform_destination_id);
    assert_eq!(go_credential["hasSecret"], true);
    assert_eq!(custom_credential["hasSecret"], true);
    assert_eq!(linked_credential["hasSecret"], true);
    assert_eq!(zen_credential["hasSecret"], false);
    harness.stop();
}

#[tokio::test]
async fn cpa_v4_get_stays_secret_free() {
    let harness = start_loopback("v4-dest-cpa").await;
    let now = chrono::Utc::now();
    let inference = harness.state.encrypt_key("cpa-inference-secret").unwrap();
    let management = harness.state.encrypt_key("cpa-mgmt-secret").unwrap();
    let account = Account {
        id: CPA_ACCOUNT_ID.to_string(),
        provider_id: CPA_PROVIDER_ID.to_string(),
        credential_kind: CredentialKind::ApiKey,
        quota_scope: QuotaScope::Key,
        name: CPA_ACCOUNT_NAME.to_string(),
        username: None,
        password_cipher: None,
        key_cipher: inference.clone(),
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
    harness
        .state
        .db
        .lock()
        .upsert_cpa_integration(&account, "http://127.0.0.1:8317", &management)
        .unwrap();

    let (status, destinations, destinations_raw) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, credentials_raw) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");

    let cpa_id = destination_id_for_builtin(CPA_PROVIDER_ID);
    let cpa = find_destination(&destinations, &cpa_id);
    assert_eq!(cpa["adapter"], "cpa");
    assert_eq!(cpa["baseUrl"], "http://127.0.0.1:8317");
    assert!(cpa["observerCredentialId"].as_str().is_some());

    for secret in [
        "cpa-inference-secret",
        "cpa-mgmt-secret",
        &inference,
        &management,
    ] {
        assert!(
            !destinations_raw.contains(secret),
            "secret leaked in destinations: {destinations_raw}"
        );
        assert!(
            !credentials_raw.contains(secret),
            "secret leaked in credentials: {credentials_raw}"
        );
    }
    for blob in [&destinations_raw, &credentials_raw] {
        assert!(
            !blob.contains("key_cipher"),
            "V4 GET must not name key_cipher: {blob}"
        );
        assert!(
            !blob.contains("management_key_cipher"),
            "V4 GET must not name management_key_cipher: {blob}"
        );
    }
    harness.stop();
}

#[tokio::test]
async fn destinations_and_credentials_require_a_session() {
    let harness = start_public("v4-dest-session").await;
    for path in ["/destinations", "/credentials"] {
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
async fn refused_projection_returns_structured_409() {
    let harness = start_loopback("v4-dest-refused").await;
    let (status, custom) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Broken Custom",
                "key": "sk-dest-broken",
                "customConfig": {
                    "endpointUrl": "https://ok-dest.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "ok",
                    "upstreamModel": "ok",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let account_id = custom["account"]["id"].as_str().unwrap().to_string();

    let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "UPDATE destinations SET base_url = ''
         WHERE legacy_kind = 'custom_account' AND legacy_id = ?1",
        [&account_id],
    )
    .unwrap();
    drop(conn);

    for path in ["/destinations", "/credentials"] {
        let (status, body, _) = send_v4(&harness, Method::GET, path, &Value::Null).await;
        assert_eq!(status, StatusCode::CONFLICT, "{path} {body}");
        assert_eq!(
            body["code"], "destinationProjectionRefused",
            "{path} {body}"
        );
        let details = body["details"].as_array().expect("details array");
        assert!(
            details.iter().any(|detail| {
                detail["row"]["kind"] == "account"
                    && detail["row"]["id"] == account_id
                    && detail["row"]["providerId"] == CUSTOM_PROVIDER_ID
                    && detail["error"] == "customAccountMissingEndpoint"
                    && detail["detail"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(&account_id)
            }),
            "{path} {body}"
        );
    }
    harness.stop();
}

#[tokio::test]
async fn destination_reads_do_not_bump_revision_or_call_upstream() {
    let (upstream, calls, _stop) = start_fake_upstream(HashMap::new()).await;
    let harness = start_loopback("v4-dest-no-outbound").await;
    let (status, created) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Quiet Custom",
                "key": "sk-dest-quiet",
                "customConfig": {
                    "endpointUrl": format!("{upstream}/v1/chat/completions"),
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "quiet-model",
                    "upstreamModel": "quiet-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");

    let (status, before, _) = send_v4(&harness, Method::GET, "/contract", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    let (status, destinations, _) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, _) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    let (status, after, _) = send_v4(&harness, Method::GET, "/contract", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");

    assert_eq!(before, after);
    assert_eq!(destinations["revision"], before);
    assert_eq!(credentials["revision"], before);
    assert!(
        calls.lock().expect("fake call log").is_empty(),
        "destination reads must not issue outbound requests"
    );
    harness.stop();
}

#[tokio::test]
async fn v4_list_after_custom_create_matches_persisted_shadow() {
    let harness = start_loopback("v4-dest-shadow-match").await;
    let (status, custom) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Shadow Custom",
                "key": "sk-dest-shadow-match",
                "customConfig": {
                    "endpointUrl": "https://shadow-match.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "shadow-model",
                    "upstreamModel": "shadow-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");

    let (live, stored) = live_and_persisted(&harness);
    assert_eq!(stored, live, "4d-2 create must refresh the v50 shadow");

    let (status, destinations, _) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, _) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    assert_v4_lists_match_projection(&destinations, &credentials, &stored);
    harness.stop();
}

#[tokio::test]
async fn populated_shadow_is_v4_read_model_until_emptied() {
    let harness = start_loopback("v4-dest-shadow-stale").await;
    let (status, custom) = send_v3(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "providerId": CUSTOM_PROVIDER_ID,
                "name": "Live Custom",
                "key": "sk-dest-shadow-stale",
                "customConfig": {
                    "endpointUrl": "https://shadow-stale.example/v1/chat/completions",
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": "stale-model",
                    "upstreamModel": "stale-model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let account_id = custom["account"]["id"].as_str().unwrap().to_string();

    {
        let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "UPDATE credentials SET name = ?1 WHERE legacy_account_id = ?2",
            ["sql-stale-must-not-win", account_id.as_str()],
        )
        .unwrap();
    }

    let (live, stored) = live_and_persisted(&harness);
    assert_ne!(stored, live);
    let (status, destinations, _) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, _) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    assert_v4_lists_match_projection(&destinations, &credentials, &stored);
    assert_ne!(
        destinations["destinations"],
        serde_json::to_value(
            live.destinations
                .iter()
                .map(DestinationDto::from)
                .collect::<Vec<_>>()
        )
        .unwrap()
    );

    {
        let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute_batch(
            "DELETE FROM credential_grants;
             DELETE FROM credentials;
             DELETE FROM destination_models;
             DELETE FROM destinations;",
        )
        .unwrap();
    }
    let (live, stored) = live_and_persisted(&harness);
    assert!(stored.destinations.is_empty());
    assert!(stored.credentials.is_empty());
    let (status, destinations, _) =
        send_v4(&harness, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let (status, credentials, _) =
        send_v4(&harness, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    assert_v4_lists_match_projection(&destinations, &credentials, &live);
    harness.stop();
}
