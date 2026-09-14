//! Dashboard V3 access-key lifecycle: auth, CAS, secrecy, routing, and retired V2 paths.

use ocg_core::dashboard_v3::{
    ConnectionInfo, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, MutationAck,
};
use ocg_core::gateway_keys::PRIMARY_KEY_ID;
use ocg_core::models::{Account, AccountSetupStep, AccountType, RoutingMode};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

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

async fn get_connection(harness: &V3Harness) -> (StatusCode, Value, ConnectionInfo) {
    let (status, body) = harness
        .get_json(&format!("{}/connection", harness.v3_base))
        .await;
    let parsed = serde_json::from_value::<ConnectionInfo>(body.clone()).unwrap_or_else(|_| {
        panic!("GET /connection should deserialize, got {body}");
    });
    (status, body, parsed)
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

fn assert_secret_free_ack(body: &Value, secrets: &[&str]) -> MutationAck {
    let ack: MutationAck =
        serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("ack JSON: {body}"));
    let object = body
        .as_object()
        .expect("mutation response must be an object");
    assert_eq!(
        object.len(),
        2,
        "MutationAck must have exactly two fields: {body}"
    );
    assert_eq!(body["revision"], ack.revision);
    assert_eq!(body["processGeneration"], ack.process_generation);
    for name in json_field_names(body) {
        assert!(
            !matches!(
                name,
                "key"
                    | "gatewayKey"
                    | "gateway_key"
                    | "primaryKey"
                    | "primary_key"
                    | "value"
                    | "id"
                    | "name"
            ),
            "key mutation leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert_ne!(
                value, *secret,
                "key mutation leaked credential {secret}: {body}"
            );
        }
    }
    ack
}

async fn chat_status(harness: &V3Harness, key: &str) -> StatusCode {
    harness
        .client
        .post(format!(
            "http://127.0.0.1:{}/v1/chat/completions",
            harness.handle.port
        ))
        .header("authorization", format!("Bearer {key}"))
        .json(&json!({"model":"m","messages":[],"max_tokens":1}))
        .send()
        .await
        .unwrap()
        .status()
}

fn sample_account() -> Account {
    let now = chrono::Utc::now();
    Account {
        id: "acct-route".into(),
        provider_id: ocg_core::provider::default_provider_id(),

        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: "route".into(),
        username: None,
        password_cipher: None,
        key_cipher: "cipher".into(),
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
    }
}

fn bind_sticky(harness: &V3Harness) {
    let selected = harness.state.routing.select_account(
        &[sample_account()],
        RoutingMode::StrictPriority,
        true,
        Some("conv-keys"),
        &[],
    );
    assert!(selected.is_some());
    assert!(harness.state.routing.sticky_binding("conv-keys").is_some());
}

fn sticky_alive(harness: &V3Harness) -> bool {
    harness.state.routing.sticky_binding("conv-keys").is_some()
}

fn key_mutation_routes(id: &str) -> Vec<(Method, String)> {
    vec![
        (Method::POST, "/keys/primary/regenerate".into()),
        (Method::POST, "/keys".into()),
        (Method::PATCH, format!("/keys/{id}")),
        (Method::DELETE, format!("/keys/{id}")),
        (Method::POST, format!("/keys/{id}/regenerate")),
    ]
}

#[tokio::test]
async fn dashboard_v3_key_routes_require_the_v3_session() {
    let harness = start_public("keys-auth").await;
    let body = json!({
        "expectedRevision": 0,
        "processGeneration": 0,
        "name": "Laptop"
    });
    for (method, path) in key_mutation_routes("missing") {
        let payload = if path == "/keys" {
            body.clone()
        } else {
            json!({
                "expectedRevision": 0,
                "processGeneration": 0
            })
        };
        let (status, response) = send_json(&harness, method.clone(), &path, &payload).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_v3_error(&response, ERROR_UNAUTHORIZED);
        assert_eq!(response["currentRevision"], Value::Null);
        assert_eq!(response["processGeneration"], Value::Null);
    }

    let v2 = harness
        .client
        .post(format!("{}/settings/keys", harness.v2_base))
        .json(&json!({ "name": "Laptop" }))
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
async fn dashboard_v3_v2_login_cookie_authorizes_key_mutations() {
    let harness = start_public("keys-cookie").await;
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

    let create = harness
        .client
        .post(format!("{}/keys", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas(&harness, json!({ "name": "Laptop" })))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);
    let body: Value = create.json().await.unwrap();
    assert_secret_free_ack(&body, &[]);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_key_mutations_require_cas_tokens() {
    let harness = start_loopback("keys-missing-cas").await;
    let before = harness.state.settings_revision();
    let primary = harness.state.config().gateway_key.clone();
    let routes = key_mutation_routes("missing");

    for (method, path) in &routes {
        let (status, body) = send_raw(
            &harness,
            method.clone(),
            path,
            &json!({ "processGeneration": harness.state.process_generation(), "name": "Laptop" })
                .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
        assert_eq!(harness.state.settings_revision(), before);
        assert_eq!(harness.state.config().gateway_key, primary);
    }

    for (method, path) in &routes {
        let (status, body) = send_raw(
            &harness,
            method.clone(),
            path,
            &json!({ "expectedRevision": before, "name": "Laptop" }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_INVALID_JSON);
        assert_eq!(harness.state.settings_revision(), before);
    }

    for (method, path) in &routes {
        let (status, body) = send_raw(&harness, method.clone(), path, "not-json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_INVALID_JSON);
        assert_eq!(harness.state.settings_revision(), before);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_expectation_only_key_mutations_reject_unknown_fields() {
    let harness = start_loopback("keys-expectation-fields").await;
    let before = harness.state.settings_revision();
    let primary = harness.state.config().gateway_key.clone();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/keys/primary/regenerate",
        &cas(&harness, json!({ "value": "must-not-be-accepted" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.config().gateway_key, primary);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_rejects_key_mutations() {
    let harness = start_loopback("keys-stale-cas").await;
    let current_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();
    let routes = key_mutation_routes("missing");

    for (method, path) in &routes {
        let mut payload = json!({
            "expectedRevision": current_revision.saturating_sub(1),
            "processGeneration": generation
        });
        if path == "/keys" {
            payload["name"] = json!("Laptop");
        }
        let (status, body) = send_json(&harness, method.clone(), path, &payload).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_REVISION_CONFLICT);
        assert_eq!(body["currentRevision"], current_revision);
        assert_eq!(body["processGeneration"], generation);
        assert_eq!(harness.state.settings_revision(), current_revision);
        assert_eq!(harness.state.config().gateway_key, primary);
    }

    for (method, path) in &routes {
        let mut payload = json!({
            "expectedRevision": current_revision,
            "processGeneration": generation ^ 1
        });
        if path == "/keys" {
            payload["name"] = json!("Laptop");
        }
        let (status, body) = send_json(&harness, method.clone(), path, &payload).await;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path} {body}");
        assert_v3_error(&body, ERROR_REVISION_CONFLICT);
        assert_eq!(body["currentRevision"], current_revision);
        assert_eq!(body["processGeneration"], generation);
        assert_eq!(harness.state.process_generation(), generation);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_create_rename_enable_disable_delete_and_regenerate() {
    let harness = start_loopback("keys-lifecycle").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "Laptop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let ack = assert_secret_free_ack(&created, &[&primary]);
    assert_eq!(ack.revision, before + 1);
    assert_eq!(ack.process_generation, generation);
    assert_eq!(harness.state.settings_revision(), before + 1);

    let (status, _, connection) = get_connection(&harness).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connection.primary_key, primary);
    assert_eq!(connection.sub_keys.len(), 1);
    let sub_id = connection.sub_keys[0].id.clone();
    let sub_value = connection.sub_keys[0].value.clone();
    assert_eq!(connection.sub_keys[0].name, "Laptop");
    assert!(connection.sub_keys[0].enabled);
    assert!(!sub_value.is_empty());
    assert_ne!(sub_value, primary);
    assert_secret_free_ack(&created, &[&primary, &sub_value]);
    assert_ne!(
        chat_status(&harness, &sub_value).await,
        StatusCode::UNAUTHORIZED
    );

    let (status, renamed) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "name": "Deck" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_secret_free_ack(&renamed, &[&primary, &sub_value]);
    assert_eq!(harness.state.settings_revision(), before + 2);
    let (_, _, connection) = get_connection(&harness).await;
    assert_eq!(connection.sub_keys[0].name, "Deck");
    assert_eq!(connection.sub_keys[0].value, sub_value);

    let (status, disabled) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_secret_free_ack(&disabled, &[&primary, &sub_value]);
    assert_eq!(
        chat_status(&harness, &sub_value).await,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, connection) = get_connection(&harness).await;
    assert!(!connection.sub_keys[0].enabled);
    assert_eq!(connection.sub_keys[0].value, sub_value);

    let (status, enabled) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_secret_free_ack(&enabled, &[&primary, &sub_value]);
    assert_ne!(
        chat_status(&harness, &sub_value).await,
        StatusCode::UNAUTHORIZED
    );

    let (status, regenerated) = send_json(
        &harness,
        Method::POST,
        &format!("/keys/{sub_id}/regenerate"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{regenerated}");
    assert_secret_free_ack(&regenerated, &[&primary, &sub_value]);
    assert_eq!(
        chat_status(&harness, &sub_value).await,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, connection) = get_connection(&harness).await;
    let rotated = connection.sub_keys[0].value.clone();
    assert_ne!(rotated, sub_value);
    assert_secret_free_ack(&regenerated, &[&primary, &sub_value, &rotated]);
    assert_ne!(
        chat_status(&harness, &rotated).await,
        StatusCode::UNAUTHORIZED
    );

    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert_secret_free_ack(&deleted, &[&primary, &sub_value, &rotated]);
    let (_, _, connection) = get_connection(&harness).await;
    assert!(connection.sub_keys.is_empty());
    let tombstone = harness
        .state
        .db
        .lock()
        .get_sub_gateway_key(&sub_id)
        .unwrap()
        .unwrap();
    assert!(tombstone.deleted_at.is_some());
    assert!(tombstone.key.is_empty());
    assert_eq!(tombstone.name, "Deck");
    assert_eq!(
        chat_status(&harness, &rotated).await,
        StatusCode::UNAUTHORIZED
    );

    let audits = harness.state.db.lock().list_gateway_logs(100).unwrap();
    for expected in [
        "created key `Laptop`",
        "renamed key `Laptop` to `Deck`",
        "disabled key `Deck`",
        "enabled key `Deck`",
        "regenerated key `Deck`",
        "deleted key `Deck`",
    ] {
        assert!(
            audits
                .iter()
                .any(|log| log.category == "keys" && log.message.contains(expected)),
            "missing audit containing {expected:?}"
        );
    }
    assert!(
        !audits
            .iter()
            .any(|log| log.category == "keys" && log.message.contains("gateway key")),
        "audit wording must say \"key\" only"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_primary_rotate_refetch_reveals_the_new_value() {
    let harness = start_loopback("keys-primary").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let old_primary = harness.state.config().gateway_key.clone();
    bind_sticky(&harness);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/keys/primary/regenerate",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ack = assert_secret_free_ack(&body, &[&old_primary]);
    assert_eq!(ack.revision, before + 1);
    assert_eq!(ack.process_generation, generation);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert!(
        !sticky_alive(&harness),
        "rotating the primary must reset routing"
    );

    let (_, _, connection) = get_connection(&harness).await;
    assert_ne!(connection.primary_key, old_primary);
    assert_eq!(connection.primary_key, harness.state.config().gateway_key);
    assert_secret_free_ack(&body, &[&old_primary, &connection.primary_key]);
    assert_eq!(
        chat_status(&harness, &old_primary).await,
        StatusCode::UNAUTHORIZED
    );
    assert_ne!(
        chat_status(&harness, &connection.primary_key).await,
        StatusCode::UNAUTHORIZED
    );

    let audits = harness.state.db.lock().list_gateway_logs(100).unwrap();
    assert!(audits.iter().any(|log| {
        log.category == "keys" && log.message.contains("regenerated primary key `Primary`")
    }));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_noop_patch_does_not_bump_revision() {
    let harness = start_loopback("keys-noop").await;
    let (status, _) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "Laptop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, connection) = get_connection(&harness).await;
    let sub_id = connection.sub_keys[0].id.clone();
    let before = harness.state.settings_revision();
    let audit_before = harness
        .state
        .db
        .lock()
        .list_gateway_logs(100)
        .unwrap()
        .len();
    bind_sticky(&harness);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(
            &harness,
            json!({
                "name": "  Laptop ",
                "enabled": true
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ack = assert_secret_free_ack(&body, &[]);
    assert_eq!(ack.revision, before);
    assert_eq!(harness.state.settings_revision(), before);
    assert!(sticky_alive(&harness), "no-op patch must not reset routing");
    let audit_after = harness
        .state
        .db
        .lock()
        .list_gateway_logs(100)
        .unwrap()
        .len();
    assert_eq!(audit_after, audit_before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_primary_key_cannot_be_disabled_or_deleted() {
    let harness = start_loopback("keys-primary-guard").await;
    let before = harness.state.settings_revision();
    let primary = harness.state.config().gateway_key.clone();

    for operation in ["patch", "delete"] {
        let (status, body) = if operation == "patch" {
            send_json(
                &harness,
                Method::PATCH,
                &format!("/keys/{PRIMARY_KEY_ID}"),
                &cas(&harness, json!({ "enabled": false, "name": "Nope" })),
            )
            .await
        } else {
            send_json(
                &harness,
                Method::DELETE,
                &format!("/keys/{PRIMARY_KEY_ID}"),
                &cas(&harness, json!({})),
            )
            .await
        };
        assert_eq!(status, StatusCode::BAD_REQUEST, "{operation} {body}");
        assert_v3_error(&body, ERROR_INVALID_REQUEST);
        assert_eq!(harness.state.settings_revision(), before);
        assert_eq!(harness.state.config().gateway_key, primary);
    }

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/keys/{PRIMARY_KEY_ID}/regenerate"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.config().gateway_key, primary);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_cap_uniqueness_and_collision_errors() {
    let harness = start_loopback("keys-cap").await;
    for index in 0..64 {
        ocg_core::gateway_keys::create_sub_key(&harness.state, &format!("seed-{index}"))
            .expect("keys below the ceiling should create");
    }
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "overflow" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);

    let retired = harness
        .state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .unwrap()[0]
        .id
        .clone();
    let (status, deleted) = send_json(
        &harness,
        Method::DELETE,
        &format!("/keys/{retired}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    let after_delete = harness.state.settings_revision();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "fresh" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(harness.state.settings_revision(), after_delete + 1);

    let (status, blank) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "  " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{blank}");
    assert_v3_error(&blank, ERROR_INVALID_REQUEST);

    let (status, long_name) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "x".repeat(65) })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{long_name}");
    assert_v3_error(&long_name, ERROR_INVALID_REQUEST);

    let (status, secret) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(
            &harness,
            json!({
                "name": "Forged",
                "value": "ocg-forged"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{secret}");
    assert_v3_error(&secret, ERROR_INVALID_JSON);

    let (_, _, connection) = get_connection(&harness).await;
    let target = connection
        .sub_keys
        .iter()
        .find(|key| key.enabled)
        .cloned()
        .expect("an enabled sub key");
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{}", target.id),
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    #[cfg(debug_assertions)]
    {
        harness
            .state
            .db
            .lock()
            .test_drop_access_key_unique_index()
            .unwrap();
        let mut config = harness.state.config();
        config.gateway_key = target.value.clone();
        harness.state.set_config(config).unwrap();
        let (status, collide) = send_json(
            &harness,
            Method::PATCH,
            &format!("/keys/{}", target.id),
            &cas(
                &harness,
                json!({
                    "name": "Renamed",
                    "enabled": true
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{collide}");
        assert_v3_error(&collide, ERROR_INVALID_REQUEST);
        let stored = harness
            .state
            .db
            .lock()
            .get_sub_gateway_key(&target.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.name, "Renamed");
        assert!(!stored.enabled);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_routing_resets_only_for_revoked_or_rotated_credentials() {
    let harness = start_loopback("keys-routing").await;
    let (status, _) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "Laptop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, connection) = get_connection(&harness).await;
    let sub_id = connection.sub_keys[0].id.clone();

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "name": "Deck" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(sticky_alive(&harness), "rename must not reset routing");

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "Phone" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(sticky_alive(&harness), "create must not reset routing");

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !sticky_alive(&harness),
        "disabling an authenticating key must reset routing"
    );

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::POST,
        &format!("/keys/{sub_id}/regenerate"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        sticky_alive(&harness),
        "regenerating a disabled key must not reset routing"
    );

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({ "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(sticky_alive(&harness), "enable must not reset routing");

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::POST,
        &format!("/keys/{sub_id}/regenerate"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !sticky_alive(&harness),
        "rotating an authenticating key must reset routing"
    );

    bind_sticky(&harness);
    let (status, _) = send_json(
        &harness,
        Method::DELETE,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !sticky_alive(&harness),
        "deleting an authenticating key must reset routing"
    );

    harness.stop();
}

#[tokio::test]
async fn retired_v2_key_mutations_do_not_create_or_rotate() {
    let harness = start_loopback("keys-v2-retired").await;

    harness
        .assert_v2_path_removed(
            Method::POST,
            "/settings/keys",
            Some(json!({
                "name": "V2",
                "expected_revision": harness.state.settings_revision()
            })),
        )
        .await;

    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "V3" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let (_, _, connection) = get_connection(&harness).await;
    let v3_key = connection
        .sub_keys
        .iter()
        .find(|key| key.name == "V3")
        .cloned()
        .expect("V3 key");
    assert_secret_free_ack(&created, &[&v3_key.value]);
    harness
        .assert_v2_path_removed(Method::GET, "/connection", None)
        .await;
    assert_eq!(connection.sub_keys.len(), 1);
    assert_eq!(connection.sub_keys[0].id, v3_key.id);

    let old_primary = harness.state.config().gateway_key.clone();
    let (status, rotated) = send_json(
        &harness,
        Method::POST,
        "/keys/primary/regenerate",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    harness
        .assert_v2_path_removed(Method::GET, "/settings", None)
        .await;
    let (_, _, connection) = get_connection(&harness).await;
    let new_primary = connection.primary_key.clone();
    assert_ne!(new_primary, old_primary);
    assert_secret_free_ack(
        &rotated,
        &[old_primary.as_str(), new_primary.as_str(), &v3_key.value],
    );

    harness
        .assert_v2_path_removed(
            Method::POST,
            "/settings/regenerate-gateway-key",
            Some(json!({
                "expected_revision": harness.state.settings_revision()
            })),
        )
        .await;
    let (_, _, after_rotate) = get_connection(&harness).await;
    assert_eq!(after_rotate.primary_key, new_primary);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_unknown_key_and_disabled_delete_do_not_bump() {
    let harness = start_loopback("keys-missing").await;
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        "/keys/does-not-exist",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, _) = send_json(
        &harness,
        Method::POST,
        "/keys",
        &cas(&harness, json!({ "name": "Laptop" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, connection) = get_connection(&harness).await;
    let sub_id = connection.sub_keys[0].id.clone();
    let (status, _) = send_json(
        &harness,
        Method::DELETE,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let after_delete = harness.state.settings_revision();
    bind_sticky(&harness);
    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/keys/{sub_id}"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), after_delete);
    assert!(
        sticky_alive(&harness),
        "deleting a tombstone must not reset routing"
    );

    harness.stop();
}
