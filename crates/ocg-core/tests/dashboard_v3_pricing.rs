//! Dashboard V3 pricing HTTP adapter: auth, CAS, shapes, provider facts, V2 coexistence.

use ocg_core::dashboard_v3::PricingAvailability;
use ocg_core::dashboard_v3::{
    ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, PricingRefresh, PricingSnapshot, ProviderPricing,
    install_official_pricing_fetch_error_for_tests, install_official_pricing_fetch_for_tests,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use ocg_core::kernel::pricing::SOURCE_URL;
use ocg_core::provider::{
    ANONYMOUS_FREE_OFFERING_ID, COMMAND_CODE_PROVIDER_ID, CUSTOM_API_OFFERING_ID,
    CUSTOM_PROVIDER_ID, GO_OFFERING_ID, GOAT_OFFERING_ID, OPENCODE_PROVIDER_ID,
    OPENCODE_ZEN_FREE_PROVIDER_ID, SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_OFFERING_IDS,
};
use reqwest::{Method, StatusCode};
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
    "snapshotJson",
    "snapshot_json",
];

fn cas_pricing(harness: &V3Harness, patch: Value) -> Value {
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
    body.insert(
        "expectedPricingRevision".into(),
        json!(harness.state.pricing_snapshot().revision),
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

fn assert_secret_free(body: &Value) {
    for name in json_field_names(body) {
        assert!(
            !SECRET_FIELD_NAMES.contains(&name),
            "pricing JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in ["sk-secret", "ocg-secret", "pw-secret", "user:pass@"] {
            assert!(
                !value.contains(secret),
                "pricing JSON leaked secret sample {secret}: {body}"
            );
        }
    }
}

fn assert_snapshot_shape(body: &Value, harness: &V3Harness) {
    let object = body.as_object().expect("PricingSnapshot object");
    for field in [
        "revision",
        "processGeneration",
        "pricingRevision",
        "activatedAt",
        "documentUpdatedAt",
        "sourceUrl",
        "contentHash",
        "adjustmentPolicyVersion",
        "limits",
        "models",
    ] {
        assert!(object.contains_key(field), "missing {field}: {body}");
    }
    assert_eq!(body["revision"], harness.state.settings_revision());
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );
    assert_eq!(
        body["pricingRevision"],
        harness.state.pricing_snapshot().revision
    );
    assert_eq!(body["sourceUrl"], SOURCE_URL);
    assert!(body["revision"].is_number());
    assert!(body["pricingRevision"].is_string());
    assert!(object.get("snapshotJson").is_none());
    assert!(object.get("snapshot_json").is_none());
    assert!(object.get("activated_at").is_none());
    assert!(object.get("content_hash").is_none());

    let limits = body["limits"].as_object().expect("limits");
    for field in ["window5h", "windowWeek", "windowMonth"] {
        assert!(limits.contains_key(field), "missing {field}");
        assert!(limits[field].is_number());
    }
    assert!(limits.get("window_5h").is_none());
    assert!(limits.get("window_week").is_none());
    assert!(limits.get("window_month").is_none());

    let model = &body["models"][0];
    let model_object = model.as_object().unwrap();
    for field in [
        "modelId",
        "displayName",
        "input",
        "output",
        "cacheRead",
        "cacheWrite",
        "usage",
        "quotaMultiplier",
        "minInputTokens",
        "maxInputTokens",
        "timeWindow",
        "adjustments",
    ] {
        assert!(model_object.contains_key(field), "missing {field}");
    }
    assert!(model.get("model_id").is_none());
    assert!(model.get("cache_write").is_none());
    assert!(model.get("quota_multiplier").is_none());
    assert!(model["cacheWrite"].is_null() || model["cacheWrite"].is_number());
    assert!(model["minInputTokens"].is_null() || model["minInputTokens"].is_number());
    assert!(model["maxInputTokens"].is_null() || model["maxInputTokens"].is_number());

    let parsed: PricingSnapshot = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.revision, harness.state.settings_revision());
    assert_eq!(
        parsed.pricing_revision,
        harness.state.pricing_snapshot().revision
    );
    assert_secret_free(body);
}

fn cas_tokens(harness: &V3Harness) -> (u64, u64, String) {
    (
        harness.state.settings_revision(),
        harness.state.process_generation(),
        harness.state.pricing_snapshot().revision.clone(),
    )
}

fn mutated_official(harness: &V3Harness) -> ocg_core::kernel::pricing::PricingSnapshot {
    let mut official = harness.state.pricing_snapshot().as_ref().clone();
    official.content_hash = "official-price-change".into();
    for model in &mut official.models {
        if model.model_id == "qwen3.7-plus" {
            model.quota_multiplier = 2.0;
        }
    }
    official
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[tokio::test]
async fn dashboard_v3_pricing_routes_require_the_v3_session() {
    let harness = start_public("pricing-auth").await;

    for path in [
        "/pricing",
        "/providers/opencode/go/pricing",
        "/providers/command-code/goat/pricing",
    ] {
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

    for (method, path) in [
        (Method::POST, "/pricing/refresh"),
        (Method::PUT, "/pricing/multipliers"),
    ] {
        let response = harness
            .client
            .request(method.clone(), format!("{}{path}", harness.v3_base))
            .json(&json!({
                "expectedRevision": 0,
                "processGeneration": 0,
                "expectedPricingRevision": "seed"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
        let body: Value = response.json().await.unwrap();
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
    }

    let v2 = harness
        .client
        .get(format!("{}/pricing", harness.v2_base))
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
async fn dashboard_v3_v2_login_cookie_authorizes_pricing_routes() {
    let harness = start_public("pricing-cookie").await;
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

    let pricing = harness
        .client
        .get(format!("{}/pricing", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(pricing.status(), StatusCode::OK);
    let provider = harness
        .client
        .get(format!(
            "{}/providers/{OPENCODE_PROVIDER_ID}/{GO_OFFERING_ID}/pricing",
            harness.v3_base
        ))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(provider.status(), StatusCode::OK);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_get_pricing_is_local_only_camelcase_and_secret_free() {
    let harness = start_loopback("pricing-get").await;
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), |_| {
            panic!("GET /pricing must not fetch the official document")
        });
    let primary = harness.state.config().gateway_key.clone();

    let (status, body) = harness
        .get_json(&format!("{}/pricing", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_snapshot_shape(&body, &harness);
    let grok = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "grok-4.5")
        .expect("seed grok-4.5");
    assert_eq!(grok["cacheWrite"], Value::Null);
    assert_eq!(grok["minInputTokens"], Value::Null);
    assert_eq!(grok["maxInputTokens"], Value::Null);
    assert_eq!(grok["timeWindow"], "always");
    let peak = body["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "deepseek-v4-pro" && model["timeWindow"] == "peak")
        .expect("seed peak row");
    assert_eq!(peak["timeWindow"], "peak");
    for value in json_string_values(&body) {
        assert_ne!(value, primary, "GET /pricing leaked the primary Key");
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_pricing_mutations_require_cas_tokens() {
    let harness = start_loopback("pricing-missing-cas").await;
    let before = cas_tokens(&harness);
    let _guard = install_official_pricing_fetch_error_for_tests(
        harness.state.process_generation(),
        "must not fetch when CAS is missing",
    );

    for (method, path, extra) in [
        (Method::POST, "/pricing/refresh", json!({})),
        (
            Method::PUT,
            "/pricing/multipliers",
            json!({ "multipliers": [{ "modelId": "grok-4.5", "multiplier": 4.0 }] }),
        ),
    ] {
        let mut body = extra.as_object().cloned().unwrap();
        body.insert(
            "processGeneration".into(),
            json!(harness.state.process_generation()),
        );
        body.insert(
            "expectedPricingRevision".into(),
            json!(harness.state.pricing_snapshot().revision),
        );
        let (status, response) = send_raw(
            &harness,
            method.clone(),
            path,
            &Value::Object(body).to_string(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{method} {path} {response}"
        );
        assert_v3_error(&response, ERROR_MISSING_EXPECTED_REVISION);
        assert_eq!(response["currentRevision"], Value::Null);
        assert_eq!(response["processGeneration"], Value::Null);
        assert_eq!(cas_tokens(&harness), before);
    }

    for (method, path, extra) in [
        (Method::POST, "/pricing/refresh", json!({})),
        (
            Method::PUT,
            "/pricing/multipliers",
            json!({ "multipliers": [{ "modelId": "grok-4.5", "multiplier": 4.0 }] }),
        ),
    ] {
        let mut body = extra.as_object().cloned().unwrap();
        body.insert(
            "expectedRevision".into(),
            json!(harness.state.settings_revision()),
        );
        body.insert(
            "processGeneration".into(),
            json!(harness.state.process_generation()),
        );
        let (status, response) = send_raw(
            &harness,
            method.clone(),
            path,
            &Value::Object(body).to_string(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{method} {path} {response}"
        );
        assert_v3_error(&response, ERROR_INVALID_JSON);
        assert_eq!(cas_tokens(&harness), before);
    }

    let (status, body) = send_raw(&harness, Method::POST, "/pricing/refresh", "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(cas_tokens(&harness), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_generation_or_pricing_revision_is_409() {
    let harness = start_loopback("pricing-stale-cas").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);
    let _guard = install_official_pricing_fetch_error_for_tests(
        harness.state.process_generation(),
        "must not fetch on stale CAS",
    );

    let stale_revision = json!({
        "expectedRevision": revision.saturating_sub(1),
        "processGeneration": generation,
        "expectedPricingRevision": pricing_revision,
        "multipliers": [{ "modelId": "grok-4.5", "multiplier": 3.5 }]
    });
    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &stale_revision,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision.clone())
    );

    let stale_generation = json!({
        "expectedRevision": revision,
        "processGeneration": generation ^ 1,
        "expectedPricingRevision": pricing_revision,
        "multipliers": [{ "modelId": "grok-4.5", "multiplier": 3.5 }]
    });
    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &stale_generation,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision.clone())
    );

    let stale_pricing = json!({
        "expectedRevision": revision,
        "processGeneration": generation,
        "expectedPricingRevision": format!("{pricing_revision}-stale"),
        "multipliers": [{ "modelId": "grok-4.5", "multiplier": 3.5 }]
    });
    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &stale_pricing,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision.clone())
    );

    let stale_refresh = json!({
        "expectedRevision": revision,
        "processGeneration": generation,
        "expectedPricingRevision": format!("{pricing_revision}-stale")
    });
    let (status, body) =
        send_json(&harness, Method::POST, "/pricing/refresh", &stale_refresh).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision)
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_write_conflicts_on_pricing_revision_without_bumping_u64() {
    let harness = start_loopback("pricing-v2-write").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);

    let v2 = harness
        .client
        .put(format!("{}/pricing/multipliers", harness.v2_base))
        .json(&json!({
            "expected_revision": pricing_revision,
            "multipliers": [{ "model_id": "grok-4.5", "multiplier": 3.25 }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body: Value = v2.json().await.unwrap();
    let v2_pricing_revision = v2_body["revision"].as_str().unwrap().to_string();
    assert_ne!(v2_pricing_revision, pricing_revision);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(
        harness.state.pricing_snapshot().revision,
        v2_pricing_revision
    );

    let (status, v3) = harness
        .get_json(&format!("{}/pricing", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{v3}");
    assert_eq!(v3["revision"], revision);
    assert_eq!(v3["pricingRevision"], v2_pricing_revision);
    let grok = v3["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "grok-4.5")
        .unwrap();
    assert_eq!(grok["quotaMultiplier"], 3.25);

    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &json!({
            "expectedRevision": revision,
            "processGeneration": generation,
            "expectedPricingRevision": pricing_revision,
            "multipliers": [{ "modelId": "glm-5.2", "multiplier": 1.5 }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(
        harness.state.pricing_snapshot().revision,
        v2_pricing_revision
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_noop_multiplier_does_not_bump_either_token() {
    let harness = start_loopback("pricing-noop").await;
    let before = cas_tokens(&harness);
    let current = harness
        .state
        .pricing_snapshot()
        .models
        .iter()
        .find(|model| model.model_id == "grok-4.5")
        .unwrap()
        .quota_multiplier;

    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &cas_pricing(
            &harness,
            json!({ "multipliers": [{ "modelId": "grok-4.5", "multiplier": current }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_snapshot_shape(&body, &harness);
    assert_eq!(cas_tokens(&harness), before);
    assert_eq!(body["pricingRevision"], before.2);

    let (status, empty) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &cas_pricing(&harness, json!({ "multipliers": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{empty}");
    assert_v3_error(&empty, ERROR_INVALID_REQUEST);
    assert_eq!(cas_tokens(&harness), before);

    let (status, unknown) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &cas_pricing(
            &harness,
            json!({ "multipliers": [{ "modelId": "not-a-model", "multiplier": 2.0 }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");
    assert_v3_error(&unknown, ERROR_INVALID_REQUEST);
    assert_eq!(cas_tokens(&harness), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_successful_multiplier_write_bumps_both_once() {
    let harness = start_loopback("pricing-write").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);

    let (status, body) = send_json(
        &harness,
        Method::PUT,
        "/pricing/multipliers",
        &cas_pricing(
            &harness,
            json!({ "multipliers": [{ "modelId": "qwen3.7-plus", "multiplier": 0.75 }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], revision + 1);
    assert_eq!(body["processGeneration"], generation);
    assert_ne!(body["pricingRevision"], pricing_revision);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    assert_eq!(harness.state.process_generation(), generation);
    assert_eq!(
        harness.state.pricing_snapshot().revision,
        body["pricingRevision"]
    );
    assert!(
        body["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["modelId"] == "qwen3.7-plus")
            .all(|model| model["quotaMultiplier"] == 0.75)
    );
    assert_snapshot_shape(&body, &harness);

    let v2 = harness
        .client
        .get(format!("{}/pricing", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(v2_body["revision"], body["pricingRevision"]);
    assert!(v2_body.get("processGeneration").is_none());
    assert!(v2_body.get("pricingRevision").is_none());
    assert!(
        v2_body["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["model_id"] == "qwen3.7-plus")
            .all(|model| model["quota_multiplier"] == 0.75)
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_failure_is_200_failed_no_change_and_preserves_lkg() {
    let harness = start_loopback("pricing-refresh-fail").await;
    let before = cas_tokens(&harness);
    let before_hash = harness.state.pricing_snapshot().content_hash.clone();
    let _guard = install_official_pricing_fetch_error_for_tests(
        harness.state.process_generation(),
        "fixture parser rejected the document",
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: PricingRefresh = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(parsed.refresh_status).unwrap(),
        json!("failed_no_change")
    );
    assert_eq!(body["refreshStatus"], "failed_no_change");
    assert_eq!(body["error"], "fixture parser rejected the document");
    assert_eq!(body["officialContentHash"], Value::Null);
    assert_eq!(body["multiplierChanges"], json!([]));
    assert_eq!(body["snapshot"]["pricingRevision"], before.2);
    assert_eq!(body["snapshot"]["contentHash"], before_hash);
    assert_eq!(cas_tokens(&harness), before);
    assert_secret_free(&body);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_confirmation_policy_and_success_bumps_both_once() {
    let harness = start_loopback("pricing-refresh-confirm").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);
    let current_plus = harness
        .state
        .pricing_snapshot()
        .models
        .iter()
        .find(|model| model.model_id == "qwen3.7-plus")
        .unwrap()
        .quota_multiplier;
    let official = mutated_official(&harness);
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), move |_| {
            Ok(official.clone())
        });

    let (status, preview) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["refreshStatus"], "needs_confirmation");
    assert_eq!(preview["officialContentHash"], "official-price-change");
    assert_eq!(preview["error"], Value::Null);
    assert_eq!(preview["multiplierChanges"].as_array().unwrap().len(), 1);
    assert_eq!(preview["multiplierChanges"][0]["modelId"], "qwen3.7-plus");
    assert_eq!(
        preview["multiplierChanges"][0]["currentMultiplier"],
        current_plus
    );
    assert_eq!(preview["multiplierChanges"][0]["officialMultiplier"], 2.0);
    assert_eq!(preview["snapshot"]["pricingRevision"], pricing_revision);
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision.clone())
    );

    let (status, stale_hash) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(
            &harness,
            json!({
                "policy": "use_official",
                "expectedOfficialContentHash": "wrong-hash"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stale_hash}");
    assert_eq!(stale_hash["refreshStatus"], "needs_confirmation");
    assert_eq!(
        cas_tokens(&harness),
        (revision, generation, pricing_revision.clone())
    );

    let (status, kept) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(
            &harness,
            json!({
                "policy": "keep_current",
                "expectedOfficialContentHash": "official-price-change"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(kept["refreshStatus"], "success");
    assert_eq!(kept["officialContentHash"], Value::Null);
    assert_eq!(kept["error"], Value::Null);
    assert_eq!(kept["snapshot"]["revision"], revision + 1);
    assert_eq!(kept["snapshot"]["processGeneration"], generation);
    assert_ne!(kept["snapshot"]["pricingRevision"], pricing_revision);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    assert_eq!(harness.state.process_generation(), generation);
    assert!(
        kept["snapshot"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["modelId"] == "qwen3.7-plus")
            .all(|model| model["quotaMultiplier"] == current_plus)
    );
    assert_eq!(kept["snapshot"]["contentHash"], "official-price-change");
    assert_snapshot_shape(&kept["snapshot"], &harness);
    assert_secret_free(&kept);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_use_official_refresh_bumps_both_and_applies_multipliers() {
    let harness = start_loopback("pricing-use-official").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);
    let official = mutated_official(&harness);
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), move |_| {
            Ok(official.clone())
        });

    let (status, preview) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["refreshStatus"], "needs_confirmation");

    let (status, applied) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(
            &harness,
            json!({
                "policy": "use_official",
                "expectedOfficialContentHash": "official-price-change"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["refreshStatus"], "success");
    assert_eq!(applied["snapshot"]["revision"], revision + 1);
    assert_eq!(applied["snapshot"]["processGeneration"], generation);
    assert_ne!(applied["snapshot"]["pricingRevision"], pricing_revision);
    assert_eq!(harness.state.settings_revision(), revision + 1);
    assert!(
        applied["snapshot"]["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|model| model["modelId"] == "qwen3.7-plus")
            .all(|model| model["quotaMultiplier"] == 2.0)
    );

    let unchanged_official = harness.state.pricing_snapshot().as_ref().clone();
    drop(_guard);
    let _unchanged =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), move |_| {
            Ok(unchanged_official.clone())
        });
    let after = cas_tokens(&harness);
    let (status, unchanged) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged["refreshStatus"], "unchanged");
    assert_eq!(cas_tokens(&harness), after);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_scoped_pricing_follows_catalog_facts() {
    let harness = start_loopback("pricing-providers").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);

    let (status, go) = harness
        .get_json(&format!(
            "{}/providers/{OPENCODE_PROVIDER_ID}/{GO_OFFERING_ID}/pricing",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{go}");
    let parsed: ProviderPricing = serde_json::from_value(go.clone()).unwrap();
    assert_eq!(parsed.availability, PricingAvailability::Available);
    assert_eq!(go["availability"], "available");
    assert_eq!(go["providerId"], OPENCODE_PROVIDER_ID);
    assert_eq!(go["offeringId"], GO_OFFERING_ID);
    assert_eq!(go["revision"], revision);
    assert_eq!(go["processGeneration"], generation);
    assert_eq!(go["pricingRevision"], pricing_revision);
    assert_eq!(go["pricingRevision"], go["snapshot"]["pricingRevision"]);
    assert!(go["snapshot"].is_object());
    assert_snapshot_shape(&go["snapshot"], &harness);
    assert!(go.get("snapshotJson").is_none());
    assert_secret_free(&go);

    let (status, zen) = harness
        .get_json(&format!(
            "{}/providers/{OPENCODE_ZEN_FREE_PROVIDER_ID}/{ANONYMOUS_FREE_OFFERING_ID}/pricing",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{zen}");
    assert_eq!(zen["availability"], "not_applicable");
    assert_eq!(zen["snapshot"], Value::Null);
    assert_eq!(zen["pricingRevision"], pricing_revision);
    assert_secret_free(&zen);

    let (status, goat) = harness
        .get_json(&format!(
            "{}/providers/{COMMAND_CODE_PROVIDER_ID}/{GOAT_OFFERING_ID}/pricing",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{goat}");
    assert_eq!(goat["availability"], "unavailable");
    assert_eq!(goat["snapshot"], Value::Null);
    assert_secret_free(&goat);

    for offering_id in SCNET_TOKEN_PLAN_OFFERING_IDS {
        let (status, scnet) = harness
            .get_json(&format!(
                "{}/providers/{SCNET_PROVIDER_ID}/{offering_id}/pricing",
                harness.v3_base
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{scnet}");
        assert_eq!(scnet["availability"], "unavailable");
        assert_eq!(scnet["snapshot"], Value::Null);
        assert_secret_free(&scnet);
    }

    let (status, custom) = harness
        .get_json(&format!(
            "{}/providers/{CUSTOM_PROVIDER_ID}/{CUSTOM_API_OFFERING_ID}/pricing",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    assert_eq!(custom["availability"], "unpriced");
    assert_eq!(custom["snapshot"], Value::Null);
    assert_secret_free(&custom);

    let (status, missing) = harness
        .get_json(&format!("{}/providers/unknown/go/pricing", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_v3_error(&missing, ERROR_NOT_FOUND);
    assert_eq!(missing["currentRevision"], revision);

    let v2_goat = harness
        .client
        .get(format!(
            "{}/providers/{COMMAND_CODE_PROVIDER_ID}/{GOAT_OFFERING_ID}/pricing",
            harness.v2_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_goat.status(), StatusCode::OK);
    let v2_goat: Value = v2_goat.json().await.unwrap();
    assert_eq!(v2_goat["availability"], "unavailable");

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_unchanged_refresh_does_not_bump() {
    let harness = start_loopback("pricing-unchanged").await;
    let before = cas_tokens(&harness);
    let mut fetched = harness.state.pricing_snapshot().as_ref().clone();
    fetched.revision = "volatile-fetch-revision".into();
    fetched.activated_at = "2099-01-01T00:00:00Z".into();
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), move |_| {
            Ok(fetched.clone())
        });

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refreshStatus"], "unchanged");
    assert_eq!(body["snapshot"]["pricingRevision"], before.2);
    assert_eq!(cas_tokens(&harness), before);

    harness.stop();
}

#[test]
fn dashboard_v3_mod_reexports_official_fetch_installer_only_under_debug_assertions() {
    let source = include_str!("../src/dashboard_v3/mod.rs");
    let idx = source
        .find("install_official_pricing_fetch_for_tests")
        .expect("installer re-export");
    let before = &source[..idx];
    let cfg_idx = before
        .rfind("#[cfg(debug_assertions)]")
        .expect("installer re-export must be debug-only");
    assert!(
        !before[cfg_idx..].contains("#[cfg(not(debug_assertions))]"),
        "installer re-export compiled into release"
    );
}

#[tokio::test]
async fn dashboard_v3_official_fetch_seam_is_process_generation_keyed_and_removed_on_drop() {
    let first = start_loopback("pricing-seam-a").await;
    let second = start_loopback("pricing-seam-b").await;
    assert_ne!(
        first.state.process_generation(),
        second.state.process_generation()
    );

    let first_guard = install_official_pricing_fetch_error_for_tests(
        first.state.process_generation(),
        "first-harness-fetch",
    );
    let second_guard = install_official_pricing_fetch_error_for_tests(
        second.state.process_generation(),
        "second-harness-fetch",
    );

    let (status, body) = send_json(
        &first,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&first, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refreshStatus"], "failed_no_change");
    assert_eq!(body["error"], "first-harness-fetch");

    let (status, body) = send_json(
        &second,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&second, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["refreshStatus"], "failed_no_change");
    assert_eq!(body["error"], "second-harness-fetch");

    drop(first_guard);
    let _first_replaced = install_official_pricing_fetch_error_for_tests(
        first.state.process_generation(),
        "first-harness-after-drop",
    );
    let (status, body) = send_json(
        &first,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&first, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["error"], "first-harness-after-drop");
    assert_ne!(body["error"], "second-harness-fetch");
    assert_ne!(body["error"], "first-harness-fetch");

    let (status, body) = send_json(
        &second,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(&second, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["error"], "second-harness-fetch");

    drop(second_guard);
    first.stop();
    second.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_returns_409_when_fetch_activates_pricing_after_precheck() {
    let harness = start_loopback("pricing-post-fetch-cas").await;
    let (revision, generation, pricing_revision) = cas_tokens(&harness);
    let official = mutated_official(&harness);
    let _guard = install_official_pricing_fetch_for_tests(
        harness.state.process_generation(),
        move |state| {
            let mut concurrent = state.pricing_snapshot().as_ref().clone();
            for model in &mut concurrent.models {
                if model.model_id == "grok-4.5" {
                    model.quota_multiplier = 7.25;
                }
            }
            concurrent.revision = "concurrent-v2-activation".into();
            concurrent.activated_at = "2099-01-01T00:00:00Z".into();
            state
                .activate_pricing_snapshot(concurrent)
                .expect("injected fetch can activate a concurrent snapshot");
            Ok(official.clone())
        },
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/pricing/refresh",
        &cas_pricing(
            &harness,
            json!({
                "policy": "use_official",
                "expectedOfficialContentHash": "official-price-change"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(
        harness.state.pricing_snapshot().revision,
        "concurrent-v2-activation"
    );
    assert_ne!(harness.state.pricing_snapshot().revision, pricing_revision);
    let live = harness.state.pricing_snapshot();
    assert!(
        live.models
            .iter()
            .any(|model| model.model_id == "grok-4.5" && model.quota_multiplier == 7.25)
    );
    assert!(
        live.models
            .iter()
            .filter(|model| model.model_id == "qwen3.7-plus")
            .all(|model| model.quota_multiplier != 2.0)
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_provider_pricing_stays_internally_equal_under_concurrent_v2_activation() {
    let harness = start_loopback("pricing-coherent-http").await;
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    let state = harness.state.clone();
    let activator = std::thread::spawn(move || {
        let mut n = 0u64;
        while !stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
            n += 1;
            let mut snapshot = state.pricing_snapshot().as_ref().clone();
            snapshot.revision = format!("concurrent-{n}");
            snapshot.activated_at = format!("2099-01-01T00:00:{:02}Z", n % 60);
            let _ = state.activate_pricing_snapshot(snapshot);
            std::thread::yield_now();
        }
    });

    for _ in 0..40 {
        let (status, go) = harness
            .get_json(&format!(
                "{}/providers/{OPENCODE_PROVIDER_ID}/{GO_OFFERING_ID}/pricing",
                harness.v3_base
            ))
            .await;
        assert_eq!(status, StatusCode::OK, "{go}");
        assert_eq!(
            go["pricingRevision"], go["snapshot"]["pricingRevision"],
            "outer pricingRevision must match nested snapshot: {go}"
        );
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    activator.join().expect("concurrent activator");
    harness.stop();
}
