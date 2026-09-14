//! Dashboard V3 read-only observability: auth, secrecy, local-only reads, and retired V2 paths.

use chrono::{Duration, Utc};
use ocg_core::dashboard_v3::{
    ApplicationModels, DailyTokensByModel, DashboardSummary, ERROR_INVALID_REQUEST,
    ERROR_UNAUTHORIZED, ForwardLogKeys, ForwardLogModels, ForwardLogs, GatewayLogs, GatewayStatus,
    install_official_pricing_fetch_for_tests,
};
use ocg_core::gateway_keys::PRIMARY_KEY_ID;
use ocg_core::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
    ForwardLog, ForwardLogNativeAttribution, SubGatewayKey, UNATTRIBUTED_KEY_FILTER,
};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, UpstreamProtocolKind,
    default_credential_kind, default_quota_scope,
};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;
#[path = "fixtures/refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

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

const OBSERVABILITY_GETS: &[&str] = &[
    "/gateway/status",
    "/application-models",
    "/dashboard/summary",
    "/dashboard/daily-tokens-by-model",
    "/logs/gateway",
    "/logs/forward",
    "/logs/forward/models",
    "/logs/forward/keys",
];

const ACCOUNT_SECRET: &str = "sk-secret-account";

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
            "observability JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "observability JSON leaked secret {secret}: {body}"
            );
        }
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "observability JSON leaked secret {secret} in encoded JSON: {body}"
        );
    }
}

fn assert_snapshot_tokens(body: &Value, harness: &V3Harness) {
    let object = body.as_object().expect("envelope object");
    for field in ["revision", "processGeneration", "pricingRevision"] {
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
    assert!(body["revision"].is_number());
    assert!(body["pricingRevision"].is_string());
    assert!(object.get("process_generation").is_none());
    assert!(object.get("pricing_revision").is_none());
}

fn go_account(harness: &V3Harness, id: &str, key: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),

        credential_kind: default_credential_kind(),
        quota_scope: default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: harness.state.encrypt_key(key).unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: "2026-01-31".into(),
        expires_on: "2026-02-28".into(),
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

fn forward_log(account_id: &str, model: &str, cost: Option<f64>, cost_state: &str) -> ForwardLog {
    ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: model.into(),
        account_id: account_id.into(),
        account_name: account_id.into(),
        route_account_id: Some(account_id.into()),
        provider_id: Some(OPENCODE_PROVIDER_ID.into()),

        credential_account_id: Some(account_id.into()),
        client_key_id: None,
        client_key_name: None,
        status: if cost_state == "priced" || cost_state == "legacy_estimate" {
            "success".into()
        } else {
            "error".into()
        },
        http_status: Some(200),
        route: "proxy".into(),
        prompt_tokens: 10,
        completion_tokens: 20,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost,
        raw_cost_usd: cost,
        quota_debit: cost,
        effective_paid_cost_usd: cost,
        pricing_revision_id: Some("seed".into()),
        quota_multiplier: Some(1.0),
        local_adjustment_multiplier: Some(1.0),
        service_tier: None,
        cost_state: cost_state.into(),
        error_message: None,
        request_id: None,
        attempt: Some(1),
        error_source: None,
        error_stage: None,
        duration_ms: Some(5),
        diagnostic: None,
    }
}

#[tokio::test]
async fn dashboard_summary_counts_routable_goat_and_custom_accounts() {
    let harness = start_loopback("obs-summary-all-plans").await;
    refreshed_go_catalog::persist_provider_catalog(
        &harness.state,
        COMMAND_CODE_PROVIDER_ID,
        ocg_core::provider::COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS,
    );
    let (_, before_body) = harness
        .get_json(&format!("{}/dashboard/summary", harness.v3_base))
        .await;
    let before: DashboardSummary = serde_json::from_value(before_body).unwrap();

    let mut goat = go_account(&harness, "acct-goat", "sk-goat");
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    let mut disabled_goat = goat.clone();
    disabled_goat.id = "acct-goat-disabled".into();
    disabled_goat.name = disabled_goat.id.clone();
    disabled_goat.enabled = false;
    let mut unreadable_goat = goat.clone();
    unreadable_goat.id = "acct-goat-unreadable".into();
    unreadable_goat.name = unreadable_goat.id.clone();
    unreadable_goat.key_cipher = "not-a-valid-ciphertext".into();

    let mut custom = go_account(&harness, "acct-custom", "sk-custom");
    custom.provider_id = CUSTOM_PROVIDER_ID.into();
    {
        let db = harness.state.db.lock();
        db.create_account(&goat).unwrap();
        db.create_account(&disabled_goat).unwrap();
        db.create_account(&unreadable_goat).unwrap();
        db.create_account_with_contract(
            &custom,
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://example.com/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "custom-summary-model".into(),
                upstream_model: "custom-summary-upstream".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: Some("account_declared".into()),
            }],
        )
        .unwrap();
    }
    harness.state.reload_provider_contracts().unwrap();

    let (status, body) = harness
        .get_json(&format!("{}/dashboard/summary", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after: DashboardSummary = serde_json::from_value(body).unwrap();
    assert_eq!(after.total_accounts, before.total_accounts + 4);
    assert_eq!(after.available_accounts, before.available_accounts + 2);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_observability_routes_require_the_v3_session() {
    let harness = start_public("obs-auth").await;

    for path in OBSERVABILITY_GETS {
        let (status, body) = harness
            .get_json(&format!("{}{path}", harness.v3_base))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    let v2 = harness
        .client
        .get(format!("{}/gateway/status", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 session middleware must stay an empty 401, got {v2_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_observability_routes() {
    let harness = start_public("obs-cookie").await;
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

    for path in OBSERVABILITY_GETS {
        let response = harness
            .client
            .get(format!("{}{path}", harness.v3_base))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_snapshot_tokens(&body, &harness);
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_observability_gets_are_local_secret_free_and_share_one_snapshot() {
    let harness = start_loopback("obs-local").await;
    refreshed_go_catalog::persist_refreshed_go_catalog(&harness.state);
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), |_| {
            panic!("observability GET must not fetch official pricing")
        });
    let primary = harness.state.config().gateway_key.clone();
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "acct-secret", ACCOUNT_SECRET))
        .unwrap();

    let (contract_status, contract) = harness
        .get_json(&format!("{}/contract", harness.v3_base))
        .await;
    assert_eq!(contract_status, StatusCode::OK, "{contract}");
    let revision = contract["revision"].clone();
    let generation = contract["processGeneration"].clone();
    let pricing = contract["pricingRevision"].clone();

    for path in OBSERVABILITY_GETS {
        let (status, body) = harness
            .get_json(&format!("{}{path}", harness.v3_base))
            .await;
        assert_eq!(status, StatusCode::OK, "{path} {body}");
        assert_eq!(body["revision"], revision, "{path}");
        assert_eq!(body["processGeneration"], generation, "{path}");
        assert_eq!(body["pricingRevision"], pricing, "{path}");
        assert_secret_free(&body, &[ACCOUNT_SECRET, primary.as_str()]);
    }

    let (status, status_body) = harness
        .get_json(&format!("{}/gateway/status", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{status_body}");
    let parsed: GatewayStatus = serde_json::from_value(status_body.clone()).unwrap();
    assert_eq!(
        parsed.running,
        harness.state.gateway.lock().is_some(),
        "gateway status must reflect the installed listener slot"
    );
    assert_eq!(parsed.port, harness.state.active_gateway_port());
    assert!(status_body.get("key").is_none());
    assert!(status_body.as_object().unwrap().contains_key("lastError"));

    let (status, models_body) = harness
        .get_json(&format!("{}/application-models", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{models_body}");
    let models: ApplicationModels = serde_json::from_value(models_body.clone()).unwrap();
    assert!(models.models.contains(&"deepseek-v4-flash".into()));
    assert!(!models.models.contains(&"minimax-m2.7-highspeed".into()));
    assert!(!models.models.iter().any(|id| id.ends_with("-free")));
    assert!(!models.models.iter().any(|id| id.contains('/')));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stopped_gateway_status_redacts_account_secret_from_last_error() {
    let harness = start_loopback("obs-last-error").await;
    let primary = harness.state.config().gateway_key.clone();
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "acct-secret", ACCOUNT_SECRET))
        .unwrap();
    harness
        .state
        .db
        .lock()
        .log_gateway(
            "error",
            "gateway",
            &format!("listener failed with {ACCOUNT_SECRET}"),
        )
        .unwrap();

    let installed = harness.state.gateway.lock().take();
    assert!(
        harness.state.gateway.lock().is_none(),
        "gateway status lastError is only surfaced when the listener slot is empty"
    );
    drop(installed);

    let (status, body) = harness
        .get_json(&format!("{}/gateway/status", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: GatewayStatus = serde_json::from_value(body.clone()).unwrap();
    assert!(!parsed.running);
    let last_error = parsed
        .last_error
        .as_deref()
        .expect("stopped gateway must surface lastError");
    assert!(
        last_error.contains("listener failed"),
        "lastError should keep the persisted gateway error text: {last_error}"
    );
    assert!(
        last_error.contains("<redacted>"),
        "lastError should apply known-secret redaction: {last_error}"
    );
    assert!(!last_error.contains(ACCOUNT_SECRET));
    assert!(!last_error.contains(&primary));
    assert!(body.as_object().unwrap().contains_key("lastError"));
    assert!(body.get("key").is_none());
    assert_secret_free(&body, &[ACCOUNT_SECRET, primary.as_str()]);
    assert_snapshot_tokens(&body, &harness);

    harness.stop();
}

#[tokio::test]
async fn v3_observability_stays_camel_case_after_v2_retirement() {
    let harness = start_loopback("obs-v2-retired").await;
    refreshed_go_catalog::persist_refreshed_go_catalog(&harness.state);
    refreshed_go_catalog::persist_enabled_zen_catalog(&harness.state);
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "acct-go", "sk-go"))
        .unwrap();
    harness
        .state
        .db
        .lock()
        .log_forward(&forward_log("acct-go", "glm-5.2", Some(1.25), "priced"))
        .unwrap();

    harness
        .assert_v2_path_removed(reqwest::Method::GET, "/application-models", None)
        .await;
    let (v3_status, v3_models) = harness
        .get_json(&format!("{}/application-models", harness.v3_base))
        .await;
    assert_eq!(v3_status, StatusCode::OK);
    assert!(v3_models["models"].as_array().is_some(), "{v3_models}");
    assert_snapshot_tokens(&v3_models, &harness);

    harness
        .assert_v2_path_removed(reqwest::Method::GET, "/gateway/status", None)
        .await;
    let (v3_status, v3_gateway) = harness
        .get_json(&format!("{}/gateway/status", harness.v3_base))
        .await;
    assert_eq!(v3_status, StatusCode::OK);
    assert_eq!(
        v3_gateway["upstreamBaseUrl"],
        ocg_core::gateway::free_models::opencode_go_base_url(
            &harness.state.config().upstream_base_url
        )
    );
    assert!(v3_gateway.get("key").is_none());
    assert!(v3_gateway.get("upstream_base_url").is_none());

    harness
        .assert_v2_path_removed(reqwest::Method::GET, "/dashboard/summary", None)
        .await;
    let (v3_status, v3_summary) = harness
        .get_json(&format!("{}/dashboard/summary", harness.v3_base))
        .await;
    assert_eq!(v3_status, StatusCode::OK);
    assert!(v3_summary.get("total_accounts").is_none());
    let parsed: DashboardSummary = serde_json::from_value(v3_summary).unwrap();
    assert!(parsed.total_accounts >= 2);
    assert!(parsed.available_accounts >= 2);
    assert!((parsed.today_cost - 1.25).abs() < 1e-9);

    harness
        .assert_v2_path_removed(
            reqwest::Method::GET,
            "/dashboard/daily-tokens-by-model?days=30",
            None,
        )
        .await;
    let (v3_status, v3_daily) = harness
        .get_json(&format!(
            "{}/dashboard/daily-tokens-by-model?days=30",
            harness.v3_base
        ))
        .await;
    assert_eq!(v3_status, StatusCode::OK);
    let daily: DailyTokensByModel = serde_json::from_value(v3_daily).unwrap();
    assert_eq!(daily.items.len(), 1);
    assert_eq!(daily.items[0].model, "glm-5.2");
    assert_eq!(daily.items[0].tokens, 30);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_application_models_follow_current_routeable_intersection() {
    let harness = start_loopback("obs-app-models").await;
    refreshed_go_catalog::persist_refreshed_go_catalog(&harness.state);
    let _guard =
        install_official_pricing_fetch_for_tests(harness.state.process_generation(), |_| {
            panic!("GET /application-models must not fetch official pricing")
        });

    let (status, body) = harness
        .get_json(&format!("{}/application-models", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let models: ApplicationModels = serde_json::from_value(body).unwrap();
    assert!(models.models.contains(&"minimax-m2.7".into()));
    assert!(!models.models.contains(&"minimax-m2.7-highspeed".into()));
    assert!(!models.models.contains(&"glm-5".into()));

    let mut pricing = harness.state.pricing_snapshot().as_ref().clone();
    pricing.models.retain(|model| model.model_id == "grok-4.5");
    pricing.revision = format!("test-app-models-{}", Utc::now().timestamp_micros());
    pricing.activated_at = Utc::now().to_rfc3339();
    harness.state.activate_pricing_snapshot(pricing).unwrap();
    let (status, body) = harness
        .get_json(&format!("{}/application-models", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"], json!(["grok-4.5"]));

    let mut empty = harness.state.pricing_snapshot().as_ref().clone();
    empty.models.clear();
    empty.revision = format!("test-app-models-empty-{}", Utc::now().timestamp_micros());
    empty.activated_at = Utc::now().to_rfc3339();
    harness.state.activate_pricing_snapshot(empty).unwrap();
    let (status, body) = harness
        .get_json(&format!("{}/application-models", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["models"], json!([]));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_summary_and_daily_cost_match_seeded_logs() {
    let harness = start_loopback("obs-summary").await;
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "acct-go", "sk-go"))
        .unwrap();
    {
        let db = harness.state.db.lock();
        let mut today_a = forward_log("acct-go", "glm-5.2", Some(1.0), "priced");
        today_a.timestamp = Utc::now();
        let mut today_b = forward_log("acct-go", "kimi-k2.7-code", Some(2.0), "legacy_estimate");
        today_b.timestamp = Utc::now();
        let mut yesterday = forward_log("acct-go", "glm-5.2", Some(3.0), "priced");
        yesterday.timestamp = Utc::now() - Duration::days(1);
        // Zero tokens keeps this row excluded from the token chart, matching the
        // old "skipped" semantics even though cost_state is not_applicable.
        let mut skipped = forward_log("acct-go", "glm-5.2", Some(9.0), "not_applicable");
        skipped.prompt_tokens = 0;
        skipped.completion_tokens = 0;
        db.log_forward(&today_a).unwrap();
        db.log_forward(&today_b).unwrap();
        db.log_forward(&yesterday).unwrap();
        db.log_forward(&skipped).unwrap();
    }

    let (v3_status, v3_summary) = harness
        .get_json(&format!("{}/dashboard/summary", harness.v3_base))
        .await;
    assert_eq!(v3_status, StatusCode::OK, "{v3_summary}");
    assert!((v3_summary["todayCost"].as_f64().unwrap() - 3.0).abs() < 1e-9);
    assert!((v3_summary["weekCost"].as_f64().unwrap() - 6.0).abs() < 1e-9);

    let (status, daily) = harness
        .get_json(&format!(
            "{}/dashboard/daily-tokens-by-model?days=3",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{daily}");
    let items = daily["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    // forward_log seeds prompt_tokens=10, completion_tokens=20 -> 30 tokens each.
    assert!(
        items
            .iter()
            .any(|row| row["model"] == "kimi-k2.7-code" && row["tokens"].as_i64().unwrap() == 30)
    );
    // The skipped row has zero tokens, so its 9.0 cost is not reflected.
    assert!(!items.iter().any(|row| row["tokens"].as_i64().unwrap() == 0));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_gateway_and_forward_logs_paginate_filter_and_redact() {
    let harness = start_loopback("obs-logs").await;
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "selected", ACCOUNT_SECRET))
        .unwrap();
    harness
        .state
        .db
        .lock()
        .create_account(&go_account(&harness, "other", "sk-other"))
        .unwrap();

    {
        let db = harness.state.db.lock();
        db.log_gateway_diagnostic(
            "error",
            "gateway",
            &format!("legacy gateway log echoed {ACCOUNT_SECRET}"),
            Some("req-secret"),
            Some(1),
            None,
            None,
            None,
            Some(
                &json!({ "upstream_error": { "message": format!("rejected {ACCOUNT_SECRET}") } })
                    .to_string(),
            ),
        )
        .unwrap();
        db.log_gateway("info", "gateway", "started").unwrap();
        db.log_gateway("info", "gateway", "idle").unwrap();

        let mut selected = forward_log("selected", "glm-5.2", Some(0.1), "priced");
        selected.prompt_tokens = 10;
        selected.completion_tokens = 20;
        selected.error_message = Some(format!("forward failed {ACCOUNT_SECRET}"));
        selected.diagnostic = Some(json!({ "message": format!("diag {ACCOUNT_SECRET}") }));
        selected.request_id = Some("req-forward".into());
        let selected_id = db.log_forward(&selected).unwrap();
        db.set_forward_log_native_attribution(
            selected_id,
            &ForwardLogNativeAttribution {
                requested_model: Some("GLM-5.2".into()),
                resolved_alias: Some("glm-5.2".into()),
                upstream_model: Some("glm-5.2-upstream".into()),
                native_cost_value: Some(0.1),
                native_cost_unit: Some("usd".into()),
                native_cost_currency: Some("USD".into()),
            },
        )
        .unwrap();

        let mut other = forward_log("other", "grok-4.5", Some(0.2), "priced");
        other.prompt_tokens = 100;
        db.log_forward(&other).unwrap();
    }

    let (status, gateway) = harness
        .get_json(&format!("{}/logs/gateway?limit=2", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{gateway}");
    let parsed: GatewayLogs = serde_json::from_value(gateway.clone()).unwrap();
    assert_eq!(parsed.items.len(), 2);
    assert_secret_free(&gateway, &[ACCOUNT_SECRET]);

    let (status, filtered) = harness
        .get_json(&format!(
            "{}/logs/gateway?requestId=req-secret",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{filtered}");
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["items"][0]["requestId"], "req-secret");
    assert!(!filtered.to_string().contains(ACCOUNT_SECRET));
    assert!(
        !filtered["items"][0]["diagnostic"]["upstream_error"]["message"]
            .as_str()
            .unwrap()
            .contains(ACCOUNT_SECRET)
    );

    harness
        .assert_v2_path_removed(
            reqwest::Method::GET,
            "/logs/forward?limit=1&offset=0&status=success&account_id=selected",
            None,
        )
        .await;
    let (v3_status, v3_page) = harness
        .get_json(&format!(
            "{}/logs/forward?limit=1&offset=0&status=success&accountId=selected",
            harness.v3_base
        ))
        .await;
    assert_eq!(v3_status, StatusCode::OK, "{v3_page}");
    assert_eq!(v3_page["items"].as_array().unwrap().len(), 1);
    assert_eq!(v3_page["summary"]["totalRequests"], 1);
    assert_eq!(v3_page["items"][0]["accountId"], "selected");
    assert_eq!(v3_page["items"][0]["requestedModel"], "GLM-5.2");
    assert_eq!(v3_page["items"][0]["resolvedAlias"], "glm-5.2");
    assert_eq!(v3_page["items"][0]["upstreamModel"], "glm-5.2-upstream");
    assert!(v3_page.get("requestedAlias").is_none());
    assert!(v3_page["items"][0].get("requestedAlias").is_none());
    assert_secret_free(&v3_page, &[ACCOUNT_SECRET]);
    let parsed: ForwardLogs = serde_json::from_value(v3_page).unwrap();
    assert_eq!(parsed.items[0].route, "proxy");

    let (status, bad) = harness
        .get_json(&format!("{}/logs/forward?sortBy=costt", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    assert_v3_error(&bad, ERROR_INVALID_REQUEST);

    let (status, snake) = harness
        .get_json(&format!(
            "{}/logs/forward?account_id=selected",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{snake}");
    assert_v3_error(&snake, ERROR_INVALID_REQUEST);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_forward_key_filters_keep_disabled_deleted_and_dangling_identities() {
    let harness = start_loopback("obs-keys").await;
    let now = Utc::now();
    {
        let db = harness.state.db.lock();
        db.insert_sub_gateway_key(&SubGatewayKey {
            id: "sub-enabled".into(),
            name: "Deck".into(),
            key: "ocg-enabled".into(),
            enabled: true,
            deleted_at: None,
            created_at: now,
        })
        .unwrap();
        db.insert_sub_gateway_key(&SubGatewayKey {
            id: "sub-disabled".into(),
            name: "Laptop".into(),
            key: "ocg-disabled".into(),
            enabled: false,
            deleted_at: None,
            created_at: now,
        })
        .unwrap();
        db.insert_sub_gateway_key(&SubGatewayKey {
            id: "sub-deleted".into(),
            name: "Phone".into(),
            key: String::new(),
            enabled: false,
            deleted_at: Some(now),
            created_at: now,
        })
        .unwrap();

        let mut enabled = forward_log("acct", "glm-5.2", Some(0.1), "priced");
        enabled.client_key_id = Some("sub-enabled".into());
        enabled.client_key_name = Some("Deck".into());
        db.log_forward(&enabled).unwrap();

        let mut disabled = forward_log("acct", "glm-5.2", Some(0.1), "priced");
        disabled.client_key_id = Some("sub-disabled".into());
        disabled.client_key_name = Some("Laptop".into());
        db.log_forward(&disabled).unwrap();

        let mut deleted = forward_log("acct", "glm-5.2", Some(0.1), "priced");
        deleted.client_key_id = Some("sub-deleted".into());
        deleted.client_key_name = Some("Phone".into());
        db.log_forward(&deleted).unwrap();

        let mut dangling = forward_log("acct", "glm-5.2", Some(0.1), "priced");
        dangling.client_key_id = Some("ghost-id".into());
        dangling.client_key_name = Some("Ghost".into());
        db.log_forward(&dangling).unwrap();

        let mut primary = forward_log("acct", "glm-5.2", Some(0.1), "priced");
        primary.client_key_id = Some(PRIMARY_KEY_ID.into());
        primary.client_key_name = Some("Primary".into());
        db.log_forward(&primary).unwrap();

        let unattributed = forward_log("acct", "grok-4.5", Some(0.4), "priced");
        db.log_forward(&unattributed).unwrap();
    }

    harness
        .assert_v2_path_removed(reqwest::Method::GET, "/logs/forward/keys", None)
        .await;
    let (v3_status, v3_keys) = harness
        .get_json(&format!("{}/logs/forward/keys", harness.v3_base))
        .await;
    assert_eq!(v3_status, StatusCode::OK, "{v3_keys}");
    let parsed: ForwardLogKeys = serde_json::from_value(v3_keys.clone()).unwrap();
    let ids: Vec<&str> = parsed.keys.iter().map(|key| key.id.as_str()).collect();
    for id in [
        "sub-enabled",
        "sub-disabled",
        "sub-deleted",
        "ghost-id",
        PRIMARY_KEY_ID,
    ] {
        assert!(ids.contains(&id), "missing {id} in {ids:?}");
    }
    assert_eq!(
        parsed
            .keys
            .iter()
            .find(|key| key.id == "sub-deleted")
            .unwrap()
            .name,
        "Phone"
    );
    assert_eq!(
        parsed
            .keys
            .iter()
            .find(|key| key.id == "ghost-id")
            .unwrap()
            .name,
        "Ghost"
    );
    assert!(v3_keys["keys"].as_array().unwrap().len() >= 5);
    assert_secret_free(&v3_keys, &["ocg-enabled", "ocg-disabled"]);

    let (status, models) = harness
        .get_json(&format!("{}/logs/forward/models", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{models}");
    let models: ForwardLogModels = serde_json::from_value(models.clone()).unwrap();
    assert!(models.models.contains(&"glm-5.2".into()));
    assert!(models.models.contains(&"grok-4.5".into()));

    let (status, page) = harness
        .get_json(&format!(
            "{}/logs/forward?keyId={UNATTRIBUTED_KEY_FILTER}",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["summary"]["totalRequests"], 1);
    assert!(page["items"][0]["clientKeyId"].is_null());

    let (status, disabled_page) = harness
        .get_json(&format!(
            "{}/logs/forward?keyId=sub-disabled",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{disabled_page}");
    assert_eq!(disabled_page["summary"]["totalRequests"], 1);
    assert_eq!(disabled_page["items"][0]["clientKeyId"], "sub-disabled");

    harness.stop();
}
