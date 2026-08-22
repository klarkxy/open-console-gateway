//! Dashboard V3 connection/settings HTTP contract: auth, secrets, CAS, and V2 coexistence.

use ocg_core::dashboard_v3::{
    ConnectionInfo, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, MutationAck, Settings,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use reqwest::StatusCode;
use serde_json::{Map, Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

fn cas_patch(harness: &V3Harness, patch: Value) -> Value {
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

async fn put_json(harness: &V3Harness, body: &Value) -> (StatusCode, Value) {
    let response = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn put_raw(harness: &V3Harness, body: &str) -> (StatusCode, Value) {
    let response = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
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

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[tokio::test]
async fn dashboard_v3_connection_and_settings_require_the_v3_session() {
    let harness = start_public("settings-auth").await;

    for path in ["/connection", "/settings"] {
        let response = harness
            .client
            .get(format!("{}{path}", harness.v3_base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    let put = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
        .json(&json!({ "expectedRevision": 0, "processGeneration": 0 }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::UNAUTHORIZED);
    let body: Value = put.json().await.unwrap();
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let v2 = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
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
async fn dashboard_v3_v2_login_cookie_authorizes_connection_and_settings() {
    let harness = start_public("settings-cookie").await;
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

    let connection = harness
        .client
        .get(format!("{}/connection", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(connection.status(), StatusCode::OK);
    let settings = harness
        .client
        .get(format!("{}/settings", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(settings.status(), StatusCode::OK);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_connection_exposes_primary_key_and_settings_does_not() {
    let harness = start_loopback("settings-secrets").await;
    let created = harness
        .client
        .post(format!("{}/settings/keys", harness.v2_base))
        .json(&json!({
            "name": "Laptop",
            "expected_revision": harness.state.settings_revision()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created: Value = created.json().await.unwrap();
    let sub_value = created["key"].as_str().unwrap().to_string();
    let primary = harness.state.config().gateway_key.clone();
    assert!(!primary.is_empty());
    assert!(!sub_value.is_empty());

    let (status, connection_json) = harness
        .get_json(&format!("{}/connection", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{connection_json}");
    let connection: ConnectionInfo = serde_json::from_value(connection_json.clone()).unwrap();
    assert_eq!(connection.primary_key, primary);
    assert_eq!(connection.revision, harness.state.settings_revision());
    assert_eq!(
        connection.process_generation,
        harness.state.process_generation()
    );
    assert_eq!(connection.sub_keys.len(), 1);
    assert_eq!(connection.sub_keys[0].value, sub_value);
    assert!(connection_json.get("gatewayKey").is_none());
    assert!(connection_json.get("key").is_none());
    assert!(connection_json.get("gateway_key").is_none());
    assert_eq!(connection_json["primaryKey"], primary);

    let (status, settings_json) = harness
        .get_json(&format!("{}/settings", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{settings_json}");
    let settings: Settings = serde_json::from_value(settings_json.clone()).unwrap();
    assert_eq!(settings.revision, harness.state.settings_revision());
    assert_eq!(
        settings.process_generation,
        harness.state.process_generation()
    );
    assert_eq!(settings.auto_start, None);
    assert!(!settings.auto_start_supported);
    assert_eq!(settings.show_dock_icon, None);
    assert!(!settings.dock_visibility_supported);
    assert!(!settings.client_root_url_from_env);

    for name in json_field_names(&settings_json) {
        assert!(
            !matches!(
                name,
                "key" | "gatewayKey" | "gateway_key" | "primaryKey" | "primary_key"
            ),
            "GET /settings leaked field {name}"
        );
    }
    for value in json_string_values(&settings_json) {
        assert_ne!(value, primary, "GET /settings leaked the primary Key");
        assert_ne!(value, sub_value, "GET /settings leaked a sub Key");
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_missing_expected_revision_is_json_400_and_does_not_bump() {
    let harness = start_loopback("settings-missing-revision").await;
    let before = harness.state.settings_revision();
    let connect_timeout_secs = harness.state.config().connect_timeout_secs;

    let (status, body) = put_raw(
        &harness,
        &json!({
            "processGeneration": harness.state.process_generation(),
            "connectTimeoutSecs": 12
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(
        harness.state.config().connect_timeout_secs,
        connect_timeout_secs
    );

    let (status, body) = put_raw(&harness, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_is_json_409_and_does_not_bump() {
    let harness = start_loopback("settings-stale-cas").await;
    let current_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let connect_timeout_secs = harness.state.config().connect_timeout_secs;

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": current_revision.saturating_sub(1),
            "processGeneration": generation,
            "connectTimeoutSecs": 18
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], current_revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), current_revision);
    assert_eq!(
        harness.state.config().connect_timeout_secs,
        connect_timeout_secs
    );

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": current_revision,
            "processGeneration": generation ^ 1,
            "connectTimeoutSecs": 18
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], current_revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), current_revision);
    assert_eq!(harness.state.process_generation(), generation);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_validation_failure_does_not_bump_revision() {
    let harness = start_loopback("settings-validate").await;
    let before = harness.state.settings_revision();
    let connect_timeout_secs = harness.state.config().connect_timeout_secs;

    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "connectTimeoutSecs": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["currentRevision"], before);
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(
        harness.state.config().connect_timeout_secs,
        connect_timeout_secs
    );

    let (status, body) = put_json(
        &harness,
        &cas_patch(
            &harness,
            json!({
                "proxyMode": "list",
                "proxyUrl": "http://127.0.0.1:7890",
                "proxyListModels": []
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(
        harness.state.config().proxy_mode,
        ocg_core::models::ProxyMode::Auto
    );

    let (status, body) =
        put_json(&harness, &cas_patch(&harness, json!({ "autoStart": true }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["message"], "auto-start is unavailable in this runtime");
    assert_eq!(harness.state.settings_revision(), before);
    assert!(!harness.state.config().auto_start);

    let (status, body) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "gatewayKey": "ocg-forged" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);
    assert_ne!(harness.state.config().gateway_key, "ocg-forged");

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_successful_write_bumps_revision_exactly_once() {
    let harness = start_loopback("settings-write").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = put_json(
        &harness,
        &cas_patch(
            &harness,
            json!({
                "connectTimeoutSecs": 12,
                "conversationSticky": true,
                "routingMode": "round-robin"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ack: MutationAck = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(ack.revision, before + 1);
    assert_eq!(ack.process_generation, generation);
    assert_eq!(body["revision"], before + 1);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(harness.state.process_generation(), generation);
    assert_eq!(harness.state.config().connect_timeout_secs, 12);
    assert!(harness.state.config().conversation_sticky);
    assert_eq!(
        harness.state.config().routing_mode,
        ocg_core::models::RoutingMode::RoundRobin
    );

    let (status, settings) = harness
        .get_json(&format!("{}/settings", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings["connectTimeoutSecs"], 12);
    assert_eq!(settings["conversationSticky"], true);
    assert_eq!(settings["routingMode"], "round-robin");
    assert_eq!(settings["revision"], before + 1);
    assert_eq!(settings["processGeneration"], generation);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_list_proxy_write_validates_then_dedupes_known_ids() {
    let harness = start_loopback("settings-proxy-list").await;
    let before = harness.state.settings_revision();

    let (status, body) = put_json(
        &harness,
        &cas_patch(
            &harness,
            json!({
                "proxyMode": "list",
                "proxyUrl": "http://127.0.0.1:7890",
                "proxyListModels": ["gpt-5.6-luna", "wildcard-*"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = put_json(
        &harness,
        &cas_patch(
            &harness,
            json!({
                "proxyMode": "list",
                "proxyUrl": "http://127.0.0.1:7890",
                "proxyListDirection": "blacklist",
                "proxyListModels": ["  gpt-5.6-luna ", "grok-4.5", "gpt-5.6-luna"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let persisted = harness.state.config();
    assert_eq!(persisted.proxy_mode, ocg_core::models::ProxyMode::List);
    assert_eq!(
        persisted.proxy_list_direction,
        ocg_core::models::ProxyListDirection::Blacklist
    );
    assert_eq!(
        persisted.proxy_list_models,
        vec!["gpt-5.6-luna".to_string(), "grok-4.5".to_string()]
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_settings_write_coexists_with_v2_wire() {
    let harness = start_loopback("settings-v2-coexist").await;

    let v2_before = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_before.status(), StatusCode::OK);
    let v2_before: Value = v2_before.json().await.unwrap();
    let primary = v2_before["gateway_key"].as_str().unwrap().to_string();
    assert!(!primary.is_empty());
    assert!(v2_before.get("connect_timeout_secs").is_some());
    assert!(v2_before.get("connectTimeoutSecs").is_none());

    let (status, _) = put_json(
        &harness,
        &cas_patch(&harness, json!({ "connectTimeoutSecs": 21 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v3_revision = harness.state.settings_revision();

    let v2_after = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(v2_after["connect_timeout_secs"], 21);
    assert_eq!(v2_after["revision"], v3_revision);
    assert_eq!(v2_after["gateway_key"], primary);

    let mut v2_payload = v2_after.clone();
    v2_payload["connect_timeout_secs"] = json!(22);
    v2_payload["expected_revision"] = json!(v3_revision);
    let v2_write = harness
        .client
        .post(format!("{}/settings", harness.v2_base))
        .json(&v2_payload)
        .send()
        .await
        .unwrap();
    assert_eq!(v2_write.status(), StatusCode::OK);
    assert_eq!(harness.state.config().connect_timeout_secs, 22);
    assert_eq!(harness.state.settings_revision(), v3_revision + 1);

    let (status, settings) = harness
        .get_json(&format!("{}/settings", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings["connectTimeoutSecs"], 22);
    for value in json_string_values(&settings) {
        assert_ne!(value, primary);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_capable_host_exposes_and_writes_auto_start() {
    let harness = start_loopback("settings-auto-start").await;
    harness.state.set_auto_start_sync(|_| Ok(()));
    let before = harness.state.settings_revision();

    let (status, settings) = harness
        .get_json(&format!("{}/settings", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{settings}");
    assert_eq!(settings["autoStartSupported"], true);
    assert_eq!(settings["autoStart"], false);

    let (status, body) =
        put_json(&harness, &cas_patch(&harness, json!({ "autoStart": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert!(harness.state.config().auto_start);

    let (status, settings) = harness
        .get_json(&format!("{}/settings", harness.v3_base))
        .await;
    assert_eq!(settings["autoStart"], true);
    assert_eq!(status, StatusCode::OK);

    harness.stop();
}
