//! Dashboard V3 GET/PUT `/claude-desktop/models`: defaults, CAS, validation,
//! secrecy, retired V2 paths, and catalog append.

use ocg_core::dashboard_v3::{
    ClaudeDesktopModels, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
};
use ocg_core::models::{
    CLAUDE_DESKTOP_HAIKU_ALIAS, CLAUDE_DESKTOP_OPUS_ALIAS, CLAUDE_DESKTOP_SONNET_ALIAS,
};
use reqwest::StatusCode;
use serde_json::{Map, Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const SECRET_FIELD_NAMES: &[&str] = &[
    "key",
    "password",
    "passwordCipher",
    "keyCipher",
    "gatewayKey",
    "gateway_key",
    "primaryKey",
    "primary_key",
    "referralCode",
    "referral_code",
    "cipher",
    "apiKey",
    "api_key",
    "token",
    "secret",
];

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

async fn get_models(harness: &V3Harness) -> (StatusCode, Value) {
    harness
        .get_json(&format!("{}/claude-desktop/models", harness.v3_base))
        .await
}

async fn put_json(harness: &V3Harness, body: &Value) -> (StatusCode, Value) {
    let response = harness
        .client
        .put(format!("{}/claude-desktop/models", harness.v3_base))
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
        .put(format!("{}/claude-desktop/models", harness.v3_base))
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
            !SECRET_FIELD_NAMES.contains(&name),
            "Claude Desktop JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert_ne!(value, *secret, "leaked {secret}: {body}");
        }
        assert!(
            !value.contains("sk-secret")
                && !value.contains("ocg-secret")
                && !value.contains("pw-secret"),
            "Claude Desktop JSON leaked secret sample {value}: {body}"
        );
    }
    assert!(body.get("pricingRevision").is_none(), "{body}");
}

fn assert_unrelated_config(harness: &V3Harness, before: &ocg_core::models::AppConfig) {
    let after = harness.state.config();
    assert_eq!(after.gateway_key, before.gateway_key);
    assert_eq!(after.gateway_port, before.gateway_port);
    assert_eq!(after.upstream_base_url, before.upstream_base_url);
    assert_eq!(after.proxy_mode, before.proxy_mode);
    assert_eq!(after.proxy_url, before.proxy_url);
    assert_eq!(after.connect_timeout_secs, before.connect_timeout_secs);
    assert_eq!(after.routing_mode, before.routing_mode);
    assert_eq!(after.conversation_sticky, before.conversation_sticky);
    assert_eq!(after.auto_start, before.auto_start);
    assert_eq!(after.show_dock_icon, before.show_dock_icon);
    assert_eq!(after.opencode_invite_url, before.opencode_invite_url);
    assert_eq!(after.client_root_url, before.client_root_url);
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_requires_the_v3_session() {
    let harness = start_public("claude-desktop-auth").await;

    let (status, body) = get_models(&harness).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": 0,
            "processGeneration": 0,
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let v2 = harness
        .client
        .get(format!("{}/claude-desktop/models", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 must stay an empty 401, got {v2_body}"
    );

    let v2_put = harness
        .client
        .put(format!("{}/claude-desktop/models", harness.v2_base))
        .json(&json!({"sonnet":"glm-5.2","opus":"","haiku":""}))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_put.status(), StatusCode::UNAUTHORIZED);
    let v2_put_body = v2_put.text().await.unwrap();
    assert!(
        v2_put_body.is_empty(),
        "V2 PUT must stay an empty 401, got {v2_put_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_claude_desktop() {
    let harness = start_public("claude-desktop-cookie").await;
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

    let get = harness
        .client
        .get(format!("{}/claude-desktop/models", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let put = harness
        .client
        .put(format!("{}/claude-desktop/models", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation(),
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        }))
        .send()
        .await
        .unwrap();
    let put_status = put.status();
    let put_body = put.text().await.unwrap();
    assert_eq!(put_status, StatusCode::OK, "{put_body}");

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_loopback_trust_is_fail_closed_when_forwarded() {
    let harness = start_loopback("claude-desktop-forwarded").await;
    let (status, body) = get_models(&harness).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    for (name, value) in [
        ("x-forwarded-for", "203.0.113.10"),
        ("x-forwarded-proto", "https"),
        ("x-real-ip", "203.0.113.10"),
        ("forwarded", "for=203.0.113.10"),
    ] {
        let v3 = harness
            .client
            .get(format!("{}/claude-desktop/models", harness.v3_base))
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(v3.status(), StatusCode::UNAUTHORIZED, "{name}");
        let v3_body: Value = v3.json().await.unwrap();
        assert_v3_error(&v3_body, ERROR_UNAUTHORIZED);

        let v3_put = harness
            .client
            .put(format!("{}/claude-desktop/models", harness.v3_base))
            .header(name, value)
            .json(&json!({
                "expectedRevision": harness.state.settings_revision(),
                "processGeneration": harness.state.process_generation(),
                "sonnet": "glm-5.2",
                "opus": "",
                "haiku": ""
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(v3_put.status(), StatusCode::UNAUTHORIZED, "{name}");

        let v2 = harness
            .client
            .get(format!("{}/claude-desktop/models", harness.v2_base))
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(v2.status(), StatusCode::UNAUTHORIZED, "{name}");
        let v2_body = v2.text().await.unwrap();
        assert!(
            v2_body.is_empty(),
            "V2 must stay an empty 401 for {name}, got {v2_body}"
        );
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_defaults_are_resolved_and_local() {
    let harness = start_loopback("claude-desktop-defaults").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let primary = harness.state.config().gateway_key.clone();
    let stored = harness.state.config().claude_desktop_models.clone();
    assert_eq!(stored.sonnet, "minimax-m3");
    assert_eq!(stored.opus, "");
    assert_eq!(stored.haiku, "");

    let (status, body) = get_models(&harness).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ClaudeDesktopModels = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.sonnet, "minimax-m3");
    assert_eq!(parsed.opus, "minimax-m3");
    assert_eq!(parsed.haiku, "minimax-m3");
    assert_eq!(parsed.revision, before);
    assert_eq!(parsed.process_generation, generation);
    assert_eq!(body["revision"], before);
    assert_eq!(body["processGeneration"], generation);
    assert_secret_free(&body, &[&primary]);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.config().claude_desktop_models, stored);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_roundtrip_all_roles_and_preserves_unrelated_config() {
    let harness = start_loopback("claude-desktop-roundtrip").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let previous = harness.state.config();
    let primary = previous.gateway_key.clone();
    let port = harness.handle.port;

    let (status, body) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "glm-5.2",
                "opus": "grok-4.5",
                "haiku": "mimo-v2.5"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ClaudeDesktopModels = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.sonnet, "glm-5.2");
    assert_eq!(parsed.opus, "grok-4.5");
    assert_eq!(parsed.haiku, "mimo-v2.5");
    assert_eq!(parsed.revision, before + 1);
    assert_eq!(parsed.process_generation, generation);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(harness.state.process_generation(), generation);
    assert_secret_free(&body, &[&primary]);
    assert_unrelated_config(&harness, &previous);
    assert_eq!(harness.handle.port, port);

    let stored = harness.state.config().claude_desktop_models.clone();
    assert_eq!(stored.sonnet, "glm-5.2");
    assert_eq!(stored.opus, "grok-4.5");
    assert_eq!(stored.haiku, "mimo-v2.5");

    let (status, fetched) = get_models(&harness).await;
    assert_eq!(status, StatusCode::OK, "{fetched}");
    assert_eq!(fetched["sonnet"], "glm-5.2");
    assert_eq!(fetched["opus"], "grok-4.5");
    assert_eq!(fetched["haiku"], "mimo-v2.5");
    assert_eq!(fetched["revision"], before + 1);

    let listed = harness
        .client
        .get(format!("http://127.0.0.1:{port}/claude-desktop/v1/models"))
        .header("x-api-key", &primary)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = listed.json().await.unwrap();
    assert_eq!(listed["data"][0]["id"], CLAUDE_DESKTOP_SONNET_ALIAS);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_rejects_invalid_aliases_raw_ids_and_empty() {
    let harness = start_loopback("claude-desktop-invalid").await;
    let before = harness.state.settings_revision();
    let stored = harness.state.config().claude_desktop_models.clone();
    let previous = harness.state.config();

    for (label, payload) in [
        (
            "role alias",
            json!({
                "sonnet": CLAUDE_DESKTOP_SONNET_ALIAS,
                "opus": "",
                "haiku": ""
            }),
        ),
        (
            "opus role alias",
            json!({
                "sonnet": "glm-5.2",
                "opus": CLAUDE_DESKTOP_OPUS_ALIAS,
                "haiku": ""
            }),
        ),
        (
            "haiku role alias",
            json!({
                "sonnet": "glm-5.2",
                "opus": "",
                "haiku": CLAUDE_DESKTOP_HAIKU_ALIAS
            }),
        ),
        (
            "raw slash id",
            json!({
                "sonnet": "deepseek/deepseek-v4-flash",
                "opus": "",
                "haiku": ""
            }),
        ),
        (
            "underscore id",
            json!({
                "sonnet": "glm_5.2",
                "opus": "",
                "haiku": ""
            }),
        ),
        (
            "unknown id",
            json!({
                "sonnet": "not-a-supported-model",
                "opus": "",
                "haiku": ""
            }),
        ),
        (
            "zen free",
            json!({
                "sonnet": "mimo-v2.5-free",
                "opus": "",
                "haiku": ""
            }),
        ),
        (
            "all empty",
            json!({
                "sonnet": "",
                "opus": "",
                "haiku": ""
            }),
        ),
    ] {
        let (status, body) = put_json(&harness, &cas(&harness, payload)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
        assert_v3_error(&body, ERROR_INVALID_REQUEST);
        assert_eq!(body["currentRevision"], before, "{label}");
        assert_eq!(
            body["processGeneration"],
            harness.state.process_generation(),
            "{label}"
        );
        assert_eq!(harness.state.settings_revision(), before, "{label}");
        assert_eq!(
            harness.state.config().claude_desktop_models,
            stored,
            "{label}"
        );
    }

    assert_unrelated_config(&harness, &previous);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_rejects_unknown_and_missing_fields() {
    let harness = start_loopback("claude-desktop-unknown").await;
    let before = harness.state.settings_revision();
    let stored = harness.state.config().claude_desktop_models.clone();

    let (status, body) = put_raw(
        &harness,
        &json!({
            "processGeneration": harness.state.process_generation(),
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "opus": "",
                "haiku": ""
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = put_raw(
        &harness,
        &json!({
            "expectedRevision": harness.state.settings_revision(),
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "glm-5.2",
                "opus": "",
                "haiku": "",
                "gatewayKey": "ocg-forged"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_ne!(harness.state.config().gateway_key, "ocg-forged");

    let (status, body) = put_raw(&harness, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.config().claude_desktop_models, stored);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_stale_revision_or_generation_does_not_mutate() {
    let harness = start_loopback("claude-desktop-stale").await;
    let current_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let stored = harness.state.config().claude_desktop_models.clone();

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": current_revision.saturating_sub(1),
            "processGeneration": generation,
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], current_revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), current_revision);
    assert_eq!(harness.state.config().claude_desktop_models, stored);

    let (status, body) = put_json(
        &harness,
        &json!({
            "expectedRevision": current_revision,
            "processGeneration": generation ^ 1,
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), current_revision);
    assert_eq!(harness.state.process_generation(), generation);
    assert_eq!(harness.state.config().claude_desktop_models, stored);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_claude_desktop_noop_mapping_does_not_bump_revision() {
    let harness = start_loopback("claude-desktop-noop").await;
    let before = harness.state.settings_revision();
    let stored = harness.state.config().claude_desktop_models.clone();

    let (status, body) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "  minimax-m3  ",
                "opus": "  ",
                "haiku": ""
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sonnet"], "minimax-m3");
    assert_eq!(body["opus"], "minimax-m3");
    assert_eq!(body["haiku"], "minimax-m3");
    assert_eq!(body["revision"], before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.config().claude_desktop_models, stored);

    let (status, filled) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "minimax-m3",
                "opus": "minimax-m3",
                "haiku": "minimax-m3"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{filled}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    let persisted = harness.state.config().claude_desktop_models;
    assert_eq!(persisted.sonnet, "minimax-m3");
    assert_eq!(persisted.opus, "minimax-m3");
    assert_eq!(persisted.haiku, "minimax-m3");

    harness.stop();
}

#[tokio::test]
async fn retired_v2_claude_desktop_does_not_mutate() {
    let harness = start_loopback("claude-desktop-v2-retired").await;
    let primary = harness.state.config().gateway_key.clone();

    harness
        .assert_v2_path_removed(reqwest::Method::GET, "/claude-desktop/models", None)
        .await;

    let (status, _) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "glm-5.2",
                "opus": "",
                "haiku": "mimo-v2.5"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v3_revision = harness.state.settings_revision();

    harness
        .assert_v2_path_removed(
            reqwest::Method::PUT,
            "/claude-desktop/models",
            Some(json!({"sonnet":"","opus":"grok-4.5","haiku":""})),
        )
        .await;
    assert_eq!(harness.state.settings_revision(), v3_revision);

    let (status, _) = put_json(
        &harness,
        &cas(
            &harness,
            json!({
                "sonnet": "",
                "opus": "grok-4.5",
                "haiku": ""
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(harness.state.settings_revision(), v3_revision + 1);

    let (status, v3) = get_models(&harness).await;
    assert_eq!(status, StatusCode::OK, "{v3}");
    assert_eq!(v3["sonnet"], "grok-4.5");
    assert_eq!(v3["opus"], "grok-4.5");
    assert_eq!(v3["haiku"], "grok-4.5");
    assert_eq!(v3["revision"], v3_revision + 1);
    assert_secret_free(&v3, &[&primary]);

    let listed = harness
        .client
        .get(format!(
            "http://127.0.0.1:{}/claude-desktop/v1/models",
            harness.handle.port
        ))
        .header("x-api-key", &primary)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK);
    let listed: Value = listed.json().await.unwrap();
    assert_eq!(listed["data"][0]["id"], CLAUDE_DESKTOP_SONNET_ALIAS);
    assert_eq!(listed["data"][1]["id"], CLAUDE_DESKTOP_OPUS_ALIAS);
    assert_eq!(listed["data"][2]["id"], CLAUDE_DESKTOP_HAIKU_ALIAS);

    let settings_before = harness.state.config().claude_desktop_models.clone();
    let settings_write = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation(),
            "connectTimeoutSecs": 17
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(settings_write.status(), StatusCode::OK);
    assert_eq!(
        harness.state.config().claude_desktop_models,
        settings_before
    );
    assert_eq!(harness.state.config().connect_timeout_secs, 17);
    assert_eq!(harness.state.config().gateway_key, primary);

    harness.stop();
}
