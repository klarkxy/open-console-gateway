//! Dashboard V3 encrypted node migration: secrecy, preview, stable-ID merge,
//! atomic import, and destination-first account ordering.

use chrono::Utc;
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, ConnectionVerificationStatus,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use reqwest::header::CACHE_CONTROL;
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::time::Duration;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_on_existing_dir, start_public};

const BUNDLE_PASSWORD: &str = "migration-password-123";
const PUBLIC_ADMIN_PASSWORD: &str = "public-admin-password-123";
const GO_KEY: &str = "sk-transfer-go";
const CUSTOM_KEY: &str = "custom-transfer-key";
const GOAT_KEY: &str = "goat-transfer-key";
static MIGRATION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
) -> (StatusCode, reqwest::header::HeaderMap, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, headers, body)
}

async fn send_v4_json(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v4_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn assert_no_store(headers: &reqwest::header::HeaderMap) {
    assert_eq!(
        headers
            .get(CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
}

async fn send_json_no_store(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let (status, headers, body) = send_json(harness, method, path, body).await;
    assert_no_store(&headers);
    (status, body)
}

async fn create_source_accounts(harness: &V3Harness) {
    let (status, _, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": "Migrated Go", "key": GO_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "Migrated Custom",
                "key": CUSTOM_KEY,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": "https://api.example.com/v1/messages",
                    "upstreamProtocol": "messages"
                },
                "modelCapabilities": [{
                    "modelId": "org/migrated-model",
                    "protocol": "messages"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "Migrated GOAT",
                "key": GOAT_KEY,
                "providerId": COMMAND_CODE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ids: Vec<String> = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.provider_id != OPENCODE_ZEN_FREE_PROVIDER_ID)
        .map(|account| account.id)
        .collect();
    for id in ids {
        harness.enable_account(&id);
    }
}

fn custom_pending_but_enabled(harness: &V3Harness) -> String {
    let (id, enabled) = harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == CUSTOM_PROVIDER_ID)
        .map(|account| (account.id, account.enabled))
        .unwrap();
    if !enabled {
        harness.enable_account(&id);
    }
    let enabled = harness
        .state
        .db
        .lock()
        .get_account(&id)
        .unwrap()
        .unwrap()
        .enabled;
    assert!(
        enabled,
        "Custom pending accounts can be enabled without verify"
    );
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .account_verification_state(&id)
            .unwrap()
            .unwrap()
            .status,
        ConnectionVerificationStatus::Pending
    );
    id
}

#[tokio::test]
async fn encrypted_account_migration_moves_keys_without_exposing_them() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("account-transfer-source").await;

    let oversized = source
        .client
        .post(format!("{}/accounts/transfer/preview", source.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("x".repeat(4 * 1024 * 1024 + 1))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_no_store(oversized.headers());

    create_source_accounts(&source).await;
    let custom_id = custom_pending_but_enabled(&source);
    let source_go_id = source
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == OPENCODE_PROVIDER_ID)
        .unwrap()
        .id;
    let source_goat_id = source
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == COMMAND_CODE_PROVIDER_ID)
        .unwrap()
        .id;
    let source_primary = source.state.config().gateway_key;
    let source_sub_key = {
        let _settings = source.state.settings_update.lock();
        ocg_core::gateway_keys::create_sub_key(&source.state, "Migrated client").unwrap()
    };

    let (status, body) = send_json_no_store(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({
            "bundlePassword": BUNDLE_PASSWORD
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["exportedAccounts"], 3);
    assert_eq!(body["skippedAccounts"], 0);
    let encoded = body.to_string();
    assert!(!encoded.contains(GO_KEY));
    assert!(!encoded.contains(CUSTOM_KEY));
    assert!(!encoded.contains(GOAT_KEY));
    let bundle = body["bundle"].as_str().unwrap().to_string();

    let (status, body) = send_json_no_store(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({
            "bundlePassword": "too-short"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let target = start_loopback("account-transfer-target").await;
    let (status, _, body) = send_json(
        &target,
        Method::POST,
        "/accounts",
        &cas(
            &target,
            json!({ "name": "Migrated Go", "key": "sk-target-extra" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let target_extra_id = body["account"]["id"].as_str().unwrap().to_string();
    let (status, preview) = send_json_no_store(
        &target,
        Method::POST,
        "/accounts/transfer/preview",
        &json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["importableAccounts"], 3);
    assert_eq!(preview["duplicateAccounts"], 0);
    assert_eq!(preview["items"].as_array().unwrap().len(), 3);
    assert!(!preview.to_string().contains(GO_KEY));
    assert!(!preview.to_string().contains(CUSTOM_KEY));

    let preview_url = format!("{}/accounts/transfer/preview", target.v3_base);
    let first = target
        .client
        .post(&preview_url)
        .json(&json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }));
    let second = target
        .client
        .post(&preview_url)
        .json(&json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }));
    let (first, second) = tokio::join!(first.send(), second.send());
    let first = first.unwrap();
    let second = second.unwrap();
    assert_no_store(first.headers());
    assert_no_store(second.headers());
    let mut statuses = [first.status(), second.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE]);

    let stale_request = cas(
        &target,
        json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
    );
    let client = target.client.clone();
    let import_url = format!("{}/accounts/transfer/import", target.v3_base);
    let stale_import = tokio::spawn(async move {
        client
            .post(import_url)
            .json(&stale_request)
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    target.state.bump_settings_revision();
    let stale_import = stale_import.await.unwrap();
    assert_eq!(stale_import.status(), StatusCode::CONFLICT);
    assert_no_store(stale_import.headers());
    assert_eq!(target.state.db.lock().list_accounts().unwrap().len(), 2);

    let before = target.state.settings_revision();
    let (status, imported) = send_json_no_store(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["importedAccounts"], 3);
    assert_eq!(imported["duplicateAccounts"], 0);
    assert_eq!(target.state.settings_revision(), before + 1);
    assert!(!imported.to_string().contains(GO_KEY));
    assert!(!imported.to_string().contains(CUSTOM_KEY));

    let accounts = target.state.db.lock().list_accounts().unwrap();
    let ordinary: Vec<_> = accounts
        .iter()
        .filter(|account| account.provider_id != OPENCODE_ZEN_FREE_PROVIDER_ID)
        .collect();
    assert_eq!(ordinary.len(), 4);
    assert_eq!(
        accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        [
            ZEN_FREE_ACCOUNT_ID,
            target_extra_id.as_str(),
            source_go_id.as_str(),
            custom_id.as_str(),
            source_goat_id.as_str(),
        ]
    );
    let go = ordinary
        .iter()
        .find(|account| account.id == source_go_id)
        .unwrap();
    assert_eq!(target.state.decrypt_key(&go.key_cipher).unwrap(), GO_KEY);
    assert!(go.enabled);
    let target_extra = ordinary
        .iter()
        .find(|account| account.id == target_extra_id)
        .unwrap();
    assert_eq!(
        target.state.decrypt_key(&target_extra.key_cipher).unwrap(),
        "sk-target-extra"
    );
    let custom = ordinary
        .iter()
        .find(|account| account.id == custom_id)
        .unwrap();
    assert_eq!(
        target.state.decrypt_key(&custom.key_cipher).unwrap(),
        CUSTOM_KEY
    );
    assert!(
        custom.enabled,
        "pending Custom accounts should remain usable"
    );
    let goat = ordinary
        .iter()
        .find(|account| account.id == source_goat_id)
        .unwrap();
    assert_eq!(
        target.state.decrypt_key(&goat.key_cipher).unwrap(),
        GOAT_KEY
    );
    let custom_contract = target
        .state
        .db
        .lock()
        .load_account_contract(&custom.id)
        .unwrap();
    assert_eq!(custom_contract.model_capabilities[0].source, "manual");
    let (_, listed) = target
        .get_json(&format!("{}/account-records", target.v3_base))
        .await;
    let custom_view = listed["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["providerId"] == CUSTOM_PROVIDER_ID)
        .unwrap();
    assert_eq!(custom_view["verificationStatus"], "pending");

    assert_eq!(target.state.config().gateway_key, source_primary);
    let target_sub_keys = target
        .state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .unwrap();
    let migrated_sub_key = target_sub_keys
        .iter()
        .find(|key| key.id == source_sub_key.id)
        .unwrap();
    assert_eq!(migrated_sub_key.name, source_sub_key.name);
    assert_eq!(migrated_sub_key.key, source_sub_key.key);
    assert_eq!(migrated_sub_key.enabled, source_sub_key.enabled);

    let revision = target.state.settings_revision();
    let (status, _, duplicate) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{duplicate}");
    assert_eq!(duplicate["importedAccounts"], 3);
    assert_eq!(duplicate["duplicateAccounts"], 0);
    assert!(
        duplicate["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["disposition"] == "merged")
    );
    assert_eq!(target.state.settings_revision(), revision + 1);
    assert_eq!(
        target
            .state
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        [
            ZEN_FREE_ACCOUNT_ID,
            target_extra_id.as_str(),
            source_go_id.as_str(),
            custom_id.as_str(),
            source_goat_id.as_str(),
        ]
    );

    let (status, body) = send_json_no_store(
        &target,
        Method::POST,
        "/accounts/transfer/preview",
        &json!({ "password": "wrong-bundle-password", "bundle": bundle }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(!body.to_string().contains(GO_KEY));
    assert!(!body.to_string().contains(CUSTOM_KEY));
    assert!(!body.to_string().contains(GOAT_KEY));

    let public = start_public("account-transfer-public").await;
    let unauthorized = public
        .client
        .post(format!("{}/accounts/transfer/preview", public.v3_base))
        .json(&json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_no_store(unauthorized.headers());
    let registered = public
        .client
        .post(format!("{}/auth/register", public.v2_base))
        .json(&json!({
            "username": "admin",
            "password": PUBLIC_ADMIN_PASSWORD
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), StatusCode::CREATED);
    let cookie = registered
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let export_body = json!({
        "bundlePassword": BUNDLE_PASSWORD
    });
    let insecure = public
        .client
        .post(format!("{}/accounts/transfer/export", public.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&export_body)
        .send()
        .await
        .unwrap();
    assert_eq!(insecure.status(), StatusCode::FORBIDDEN);
    assert_no_store(insecure.headers());
    let spoofed_https = public
        .client
        .post(format!("{}/accounts/transfer/export", public.v3_base))
        .header(reqwest::header::COOKIE, cookie)
        .header("x-forwarded-proto", "https")
        .json(&export_body)
        .send()
        .await
        .unwrap();
    assert_eq!(spoofed_https.status(), StatusCode::FORBIDDEN);
    assert_no_store(spoofed_https.headers());

    let collision_target = start_loopback("account-transfer-key-collision-target").await;
    let collision_primary = collision_target.state.config().gateway_key;
    let target_conflict = ocg_core::models::SubGatewayKey {
        id: "destination-only-key".into(),
        name: "Destination client".into(),
        key: source_sub_key.key.clone(),
        enabled: true,
        deleted_at: None,
        created_at: Utc::now(),
    };
    collision_target
        .state
        .db
        .lock()
        .insert_sub_gateway_key(&target_conflict)
        .unwrap();
    let before_order = collision_target
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect::<Vec<_>>();
    let (status, body) = send_json_no_store(
        &collision_target,
        Method::POST,
        "/accounts/transfer/preview",
        &json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(!body.to_string().contains(&source_sub_key.key));
    let (status, body) = send_json_no_store(
        &collision_target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &collision_target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        collision_target.state.config().gateway_key,
        collision_primary
    );
    assert_eq!(
        collision_target
            .state
            .db
            .lock()
            .primary_access_key_value()
            .unwrap()
            .as_deref(),
        Some(collision_primary.as_str())
    );
    let target_keys = collision_target
        .state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .unwrap();
    assert_eq!(target_keys.len(), 1);
    assert_eq!(target_keys[0].id, target_conflict.id);
    assert!(target_keys.iter().all(|key| key.id != source_sub_key.id));
    assert_eq!(
        collision_target
            .state
            .db
            .lock()
            .list_accounts()
            .unwrap()
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>(),
        before_order
    );

    source.stop();
    target.stop();
    public.stop();
    collision_target.stop();
}

#[tokio::test]
async fn node_migration_merges_dynamic_providers_by_stable_id() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("dyn-transfer-source").await;
    let (status, _, created) = send_json(
        &source,
        Method::POST,
        "/providers",
        &cas(
            &source,
            json!({
                "name": "Lab",
                "endpointUrl": "http://127.0.0.1:9/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab-source",
                "models": [{
                    "publicModel": "lab-opus",
                    "upstreamModel": "vendor/opus"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let source_provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let (status, _, exported) = send_json(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({ "bundlePassword": BUNDLE_PASSWORD }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let bundle = exported["bundle"].as_str().unwrap().to_string();

    let target = start_loopback("dyn-transfer-target").await;
    let (status, _, dest_only) = send_json(
        &target,
        Method::POST,
        "/providers",
        &cas(
            &target,
            json!({
                "name": "Lab",
                "endpointUrl": "http://127.0.0.1:10/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "key": "sk-lab-dest-only",
                "models": [{
                    "publicModel": "dest-opus",
                    "upstreamModel": "vendor/dest"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dest_only}");
    let dest_only_id = dest_only["provider"]["id"].as_str().unwrap().to_string();
    assert_ne!(dest_only_id, source_provider_id);

    let (status, _, imported) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    let target_dynamics = target.state.db.lock().list_dynamic_providers().unwrap();
    assert!(
        target_dynamics
            .iter()
            .any(|runtime| runtime.id == dest_only_id && runtime.name == "Lab"),
        "{target_dynamics:?}"
    );
    let imported_def = target_dynamics
        .iter()
        .find(|runtime| runtime.id == source_provider_id)
        .unwrap();
    assert_eq!(imported_def.endpoint_url, "http://127.0.0.1:9/v1");
    assert_eq!(imported_def.mappings[0].public_model, "lab-opus");
    let target_accounts = target.state.db.lock().list_accounts().unwrap();
    assert!(
        target_accounts
            .iter()
            .any(|account| account.provider_id == source_provider_id),
        "{target_accounts:?}"
    );
    assert!(
        target_accounts
            .iter()
            .any(|account| account.provider_id == dest_only_id),
        "{target_accounts:?}"
    );

    let (status, _, patched) = send_json(
        &target,
        Method::PATCH,
        &format!("/providers/{source_provider_id}"),
        &cas(
            &target,
            json!({
                "name": "OldLab",
                "endpointUrl": "http://127.0.0.1:11/v1",
                "upstreamProtocol": "chat_completions",
                "authKind": "bearer",
                "models": [{
                    "publicModel": "old-opus",
                    "upstreamModel": "vendor/old"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    let (status, _, merged) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    let matching_def = target
        .state
        .db
        .lock()
        .get_dynamic_provider(&source_provider_id)
        .unwrap()
        .unwrap();
    assert_eq!(matching_def.name, "Lab");
    assert_eq!(matching_def.endpoint_url, "http://127.0.0.1:9/v1");
    assert_eq!(matching_def.mappings[0].public_model, "lab-opus");
    assert!(
        target
            .state
            .db
            .lock()
            .get_dynamic_provider(&dest_only_id)
            .unwrap()
            .is_some()
    );

    source.stop();
    target.stop();
}

fn v4_base(harness: &V3Harness) -> String {
    harness.v4_base.clone()
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

fn identities_of(body: &Value) -> &[Value] {
    body["identities"].as_array().expect("identities array")
}

#[tokio::test]
async fn v6_roundtrip_preserves_shared_identity_and_binding_scopes() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("account-transfer-v6-identity-source").await;
    let (status, _, created) = send_json(
        &source,
        Method::POST,
        "/accounts",
        &cas(
            &source,
            json!({ "name": "Shared Go", "key": "sk-v6-identity-a" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let first_account_id = created["account"]["id"].as_str().unwrap().to_string();
    let (status, listed) = send_v4(&source, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let identity = identities_of(&listed)
        .iter()
        .find(|item| {
            item["legacy"]["kind"] == "account" && item["legacy"]["id"] == first_account_id
        })
        .expect("source identity");
    let identity_id = identity["identity"]["id"].as_str().unwrap().to_string();
    let first_binding = identity["credentials"][0]["bindings"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let first_grants = identity["credentials"][0]["bindings"][0]["allowedEndpointIds"].clone();
    let plan_connection = identity["credentials"][0]["bindings"][0]["connectionId"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, second) = send_v4(
        &source,
        Method::POST,
        &format!("/identities/{identity_id}/credentials"),
        &cas(
            &source,
            json!({
                "connectionId": plan_connection,
                "secretInput": "sk-v6-identity-b",
                "quotaSharing": {
                    "kind": "shared",
                    "credentialId": identity["credentials"][0]["credential"]["id"]
                }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let second_account_id = second["accountId"].as_str().unwrap().to_string();
    let second_binding = second["bindingId"].as_str().unwrap().to_string();
    let blocked_until = Utc::now() + chrono::Duration::hours(2);
    source
        .state
        .db
        .lock()
        .set_account_cooldown(
            &first_account_id,
            Some(blocked_until),
            Some("synthetic shared limit"),
        )
        .unwrap();
    let (status, patched) = send_v4(
        &source,
        Method::PATCH,
        &format!("/bindings/{second_binding}"),
        &cas(
            &source,
            json!({
                "enabled": false,
                "modelScope": { "kind": "only", "models": ["glm-5.1"] }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");

    let (status, _, exported) = send_json(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({ "bundlePassword": BUNDLE_PASSWORD }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    assert_eq!(exported["exportedAccounts"], 2);
    let bundle = exported["bundle"].as_str().unwrap().to_string();
    assert!(!bundle.contains("sk-v6-identity-a"));
    assert!(!bundle.contains("sk-v6-identity-b"));

    let target = start_loopback("account-transfer-v6-identity-target").await;
    let before = target.state.db.lock().list_accounts().unwrap().len();
    let (status, _, imported) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["importedAccounts"], 2);
    let accounts = target.state.db.lock().list_accounts().unwrap();
    assert_eq!(accounts.len(), before + 2);
    assert!(
        accounts
            .iter()
            .any(|account| account.id == first_account_id)
    );
    assert!(
        accounts
            .iter()
            .any(|account| account.id == second_account_id)
    );
    let first = accounts
        .iter()
        .find(|account| account.id == first_account_id)
        .unwrap();
    let second_account = accounts
        .iter()
        .find(|account| account.id == second_account_id)
        .unwrap();
    assert_eq!(
        target.state.decrypt_key(&first.key_cipher).unwrap(),
        "sk-v6-identity-a"
    );
    assert_eq!(
        target
            .state
            .decrypt_key(&second_account.key_cipher)
            .unwrap(),
        "sk-v6-identity-b"
    );

    let (status, after) = send_v4(&target, Method::GET, "/accounts", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let identity = identities_of(&after)
        .iter()
        .find(|item| item["identity"]["id"] == identity_id)
        .expect("imported shared identity");
    assert_eq!(identity["credentials"].as_array().unwrap().len(), 2);
    let bindings: Vec<&Value> = identity["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .map(|credential| &credential["bindings"][0])
        .collect();
    let first_imported = bindings
        .iter()
        .find(|binding| binding["id"] == first_binding)
        .unwrap();
    let second_imported = bindings
        .iter()
        .find(|binding| binding["id"] == second_binding)
        .unwrap();
    assert_eq!(first_imported["enabled"], true);
    assert_eq!(first_imported["modelScope"]["kind"], "all");
    assert_eq!(second_imported["enabled"], false);
    assert_eq!(second_imported["modelScope"]["kind"], "only");
    assert_eq!(second_imported["modelScope"]["models"][0], "glm-5.1");
    assert_eq!(first_imported["allowedEndpointIds"], first_grants);
    assert!(
        !first_imported["allowedEndpointIds"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let members = target
        .state
        .db
        .lock()
        .shared_pool_account_ids(&first_account_id)
        .unwrap();
    assert_eq!(members.len(), 2);
    assert!(members.contains(&first_account_id));
    assert!(members.contains(&second_account_id));
    for id in [&first_account_id, &second_account_id] {
        let account = target.state.db.lock().get_account(id).unwrap().unwrap();
        assert_eq!(account.cooldown_generic_until, Some(blocked_until));
        assert!(account.is_cooling_at(Utc::now()));
    }

    source.stop();
    target.stop();
}

#[tokio::test]
async fn v6_draft_provider_roundtrip_stays_off_runtime() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("draft-transfer-source").await;
    let v4_base = source.v4_base.clone();
    let mut body = json!({
        "mode": "draft",
        "operationId": "aaaaaaaa-bbbb-4ccc-8ddd-0000000000aa",
        "connection": {
            "kind": "new",
            "templateId": "custom-http",
            "name": "Draft Transfer",
            "endpointUrl": "https://draft-transfer.example/v1/chat/completions",
            "upstreamProtocol": "chat_completions",
            "authKind": "bearer"
        },
        "targets": []
    });
    body["expectedRevision"] = json!(source.state.settings_revision());
    body["processGeneration"] = json!(source.state.process_generation());
    let response = source
        .client
        .post(format!("{v4_base}/onboarding/commit"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let drafted = response.json::<Value>().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{drafted}");
    let provider_id = source
        .state
        .db
        .lock()
        .onboarding_draft_provider_ids()
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert!(source.state.dynamic_providers().is_empty());

    let (status, _, exported) = send_json(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({ "bundlePassword": BUNDLE_PASSWORD }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let bundle = exported["bundle"].as_str().unwrap().to_string();

    let target = start_loopback("draft-transfer-target").await;
    let (status, _, imported) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert!(
        target
            .state
            .dynamic_providers()
            .iter()
            .all(|runtime| runtime.id != provider_id)
    );
    assert_eq!(
        target
            .state
            .db
            .lock()
            .provider_is_onboarding_draft(&provider_id)
            .unwrap(),
        Some(true)
    );
    assert!(
        target
            .state
            .db
            .lock()
            .list_control_plane_dynamic_providers()
            .unwrap()
            .iter()
            .any(|runtime| runtime.id == provider_id && runtime.mappings.is_empty())
    );

    source.stop();
    target.stop();
}

#[derive(Debug, PartialEq, Eq)]
struct TransferTruth {
    accounts: Vec<(String, String, String, bool, String)>,
    destinations: Vec<(String, String, Option<String>, Vec<(String, String)>)>,
    routing: Vec<(String, String, u32, bool, String)>,
}

fn capture_transfer_truth(harness: &V3Harness) -> TransferTruth {
    let db = harness.state.db.lock();
    let mut accounts = db
        .list_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| {
            account.provider_id != OPENCODE_ZEN_FREE_PROVIDER_ID
                && account.id != ocg_core::provider::CPA_ACCOUNT_ID
        })
        .map(|account| {
            let key = if account.key_cipher.is_empty() {
                String::new()
            } else {
                harness.state.decrypt_key(&account.key_cipher).unwrap()
            };
            (
                account.id,
                account.provider_id,
                account.name,
                account.enabled,
                key,
            )
        })
        .collect::<Vec<_>>();
    accounts.sort();
    let stored = ocg_core::destination_projection::load_persisted(&db).unwrap();
    drop(db);
    let mut routing = stored
        .credentials
        .iter()
        .filter(|credential| {
            credential.legacy_account_id != ocg_core::provider::CPA_ACCOUNT_ID
                && credential.legacy_account_id != ZEN_FREE_ACCOUNT_ID
        })
        .map(|credential| {
            (
                credential.legacy_account_id.clone(),
                credential.destination_id.clone(),
                credential.routing_rank,
                credential.enabled,
                serde_json::to_string(&credential.scope).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    routing.sort();
    let routed_dests = routing
        .iter()
        .map(|row| row.1.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut destinations = stored
        .destinations
        .iter()
        .filter(|destination| routed_dests.contains(&destination.id))
        .map(|destination| {
            let catalog = destination
                .catalog
                .iter()
                .map(|model| (model.public_model.clone(), model.upstream_model.clone()))
                .collect::<Vec<_>>();
            (
                destination.id.clone(),
                destination.name.clone(),
                destination.base_url.clone(),
                catalog,
            )
        })
        .collect::<Vec<_>>();
    destinations.sort();
    TransferTruth {
        accounts,
        destinations,
        routing,
    }
}

#[tokio::test]
async fn v7_export_import_reopen_preserves_fields_keys_catalog_and_routing() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("v7-reopen-source").await;
    create_source_accounts(&source).await;
    let before = capture_transfer_truth(&source);
    assert_eq!(before.accounts.len(), 3);
    assert!(!before.accounts.iter().any(|row| row.4.is_empty()));
    assert!(before.destinations.iter().any(|destination| {
        destination
            .3
            .iter()
            .any(|model| model.0 == "org/migrated-model")
    }));

    let (status, _, exported) = send_json(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({ "bundlePassword": BUNDLE_PASSWORD }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let bundle = exported["bundle"].as_str().unwrap().to_string();

    let target = start_loopback("v7-reopen-target").await;
    let (status, _, imported) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({ "password": BUNDLE_PASSWORD, "bundle": bundle }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    let after_import = capture_transfer_truth(&target);
    assert_eq!(before, after_import);

    let dir = target.close_keep_dir();
    let reopened = start_on_existing_dir(dir).await;
    let after_reopen = capture_transfer_truth(&reopened);
    assert_eq!(before, after_reopen);

    source.stop();
    reopened.stop();
}

#[tokio::test]
async fn v8_roundtrip_preserves_shared_and_empty_custom_connections() {
    let _migration_guard = MIGRATION_TEST_LOCK.lock().await;
    let source = start_loopback("v8-shared-custom-source").await;
    let create_custom = |name: &str, key: &str, endpoint: &str, model: &str| {
        cas(
            &source,
            json!({
                "name": name,
                "key": key,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": endpoint,
                    "upstreamProtocol": "chat_completions"
                },
                "modelCapabilities": [{
                    "publicModel": model,
                    "upstreamModel": format!("vendor/{model}"),
                    "protocol": "chat_completions"
                }]
            }),
        )
    };
    let (status, _, first) = send_json(
        &source,
        Method::POST,
        "/accounts",
        &create_custom(
            "Shared source",
            "sk-shared-first",
            "https://shared.example/v1/chat/completions",
            "shared-model",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, connections) =
        send_v4_json(&source, Method::GET, "/connections", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let shared_connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["legacy"]["kind"] == "custom_account")
        .unwrap();
    let connection_id = shared_connection["id"].as_str().unwrap().to_string();
    let second = cas(
        &source,
        json!({
            "operationId": "aaaaaaaa-bbbb-4ccc-8ddd-00000000f801",
            "connection": { "kind": "existing", "connectionId": connection_id },
            "authorization": {
                "kind": "api_key",
                "secretInput": "sk-shared-second",
                "accountLabel": "Shared second"
            },
            "targets": []
        }),
    );
    let (status, second) = send_v4_json(&source, Method::POST, "/onboarding/commit", &second).await;
    assert_eq!(status, StatusCode::OK, "{second}");

    let (status, destinations) =
        send_v4_json(&source, Method::GET, "/destinations", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    let shared_destination = destinations["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["legacy"]["kind"] == "custom_account")
        .unwrap();
    let shared_destination_id = shared_destination["id"].as_str().unwrap().to_string();
    let (status, credentials) =
        send_v4_json(&source, Method::GET, "/credentials", &Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    let authorize_ids = credentials["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["destinationId"] == shared_destination_id)
        .map(|row| row["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(authorize_ids.len(), 2);
    let patch = cas(
        &source,
        json!({
            "name": "Shared imported name",
            "endpointUrl": "https://shared.example/v1/chat/completions",
            "upstreamProtocol": "chat_completions",
            "authScheme": "x_api_key",
            "models": [{
                "publicModel": "shared-model",
                "upstreamModel": "vendor/shared-model",
                "upstreamOverride": {
                    "protocol": "messages",
                    "endpointUrl": "https://override.example/v1/messages"
                }
            }],
            "authorizeCredentialIds": authorize_ids
        }),
    );
    let (status, patched) = send_v4_json(
        &source,
        Method::PATCH,
        &format!("/destinations/{shared_destination_id}"),
        &patch,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");

    let (status, _, empty_created) = send_json(
        &source,
        Method::POST,
        "/accounts",
        &create_custom(
            "Empty imported name",
            "sk-empty",
            "https://empty.example/v1/chat/completions",
            "empty-model",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{empty_created}");
    let empty_account_id = empty_created["account"]["id"].as_str().unwrap().to_string();
    source
        .state
        .db
        .lock()
        .delete_account(&empty_account_id)
        .unwrap();

    let (status, _, exported) = send_json(
        &source,
        Method::POST,
        "/accounts/transfer/export",
        &json!({ "bundlePassword": BUNDLE_PASSWORD }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let target = start_loopback("v8-shared-custom-target").await;
    let (status, _, imported) = send_json(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        &cas(
            &target,
            json!({
                "password": BUNDLE_PASSWORD,
                "bundle": exported["bundle"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");

    let stored = ocg_core::destination_projection::load_persisted(&target.state.db.lock()).unwrap();
    let shared = stored
        .destinations
        .iter()
        .find(|destination| destination.id == shared_destination_id)
        .unwrap();
    assert_eq!(shared.name, "Shared imported name");
    assert_eq!(shared.auth_scheme.as_str(), "x_api_key");
    assert_eq!(
        shared.catalog[0]
            .upstream_override
            .as_ref()
            .unwrap()
            .endpoint_url,
        "https://override.example/v1/messages"
    );
    assert_eq!(
        stored
            .credentials
            .iter()
            .filter(|credential| credential.destination_id == shared_destination_id)
            .count(),
        2
    );
    let empty = stored
        .destinations
        .iter()
        .find(|destination| destination.name == "Empty imported name")
        .unwrap();
    assert_eq!(
        stored
            .credentials
            .iter()
            .filter(|credential| credential.destination_id == empty.id)
            .count(),
        0
    );

    source.stop();
    target.stop();
}
