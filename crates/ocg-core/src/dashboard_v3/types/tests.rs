use super::*;

#[test]
fn cpa_oauth_method_defaults_to_browser_and_accepts_explicit_device() {
    let input =
        serde_json::json!({"provider":"codex", "expectedRevision":1, "processGeneration":1});
    let parsed: CpaOAuthStartRequest = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(parsed.method, CpaOAuthMethod::Browser);
    let mut device = input;
    device["method"] = serde_json::json!("device");
    let parsed: CpaOAuthStartRequest = serde_json::from_value(device.clone()).unwrap();
    assert_eq!(parsed.method, CpaOAuthMethod::Device);
    device["method"] = serde_json::json!("arbitrary-command");
    assert!(serde_json::from_value::<CpaOAuthStartRequest>(device).is_err());
}
use serde_json::json;

#[test]
fn wire_fields_are_camel_case() {
    let revision = ControlRevision {
        revision: 7,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    assert_eq!(
        serde_json::to_value(&revision).unwrap(),
        json!({
            "revision": 7,
            "processGeneration": 9,
            "pricingRevision": "seed" })
    );

    let parsed: MutationExpectation = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9 }))
    .unwrap();
    assert_eq!(parsed.expected_revision, 3);
    assert_eq!(parsed.process_generation, 9);
    assert!(
        serde_json::from_value::<MutationExpectation>(json!({ "expected_revision": 3 })).is_err()
    );
    assert!(
        serde_json::from_value::<MutationExpectation>(json!({
            "expectedRevision": 3,
            "processGeneration": 7,
            "value": "must-not-be-accepted"
        }))
        .is_err()
    );
}

#[test]
fn error_envelope_always_emits_nullable_fields() {
    let error = V3Error::missing_expected_revision();
    let value = serde_json::to_value(&error).unwrap();
    assert_eq!(value["code"], ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(value["currentRevision"], Value::Null);
    assert_eq!(value["processGeneration"], Value::Null);
    assert!(!value.as_object().unwrap().contains_key("current_revision"));
}

#[test]
fn schema_catalog_is_extensible_and_names_kernel_types() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    let required_error = defs["V3Error"]["required"]
        .as_array()
        .expect("V3Error.required");
    for field in ["code", "message", "currentRevision", "processGeneration"] {
        assert!(
            required_error.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
    let expectation_required = defs["MutationExpectation"]["required"]
        .as_array()
        .expect("MutationExpectation.required");
    assert_eq!(
        expectation_required,
        &vec![json!("expectedRevision"), json!("processGeneration")]
    );
    assert_eq!(schema["title"], "DashboardApiV3");
}

#[test]
fn connection_info_is_the_only_secret_bearing_dto() {
    let connection = ConnectionInfo {
        gateway_port: 9042,
        client_root_url: String::new(),
        primary_key: "ocg-secret".into(),
        sub_keys: vec![ConnectionSubKey {
            id: "sub".into(),
            name: "Laptop".into(),
            enabled: true,
            value: "ocg-sub-secret".into(),
        }],
        revision: 3,
        process_generation: 9,
    };
    let value = serde_json::to_value(&connection).unwrap();
    assert_eq!(value["primaryKey"], "ocg-secret");
    assert_eq!(value["subKeys"][0]["value"], "ocg-sub-secret");
    assert!(value.get("upstreamBaseUrl").is_none());
    assert!(value.get("gatewayKey").is_none());
    assert!(value.get("key").is_none());
    assert!(value.get("gateway_key").is_none());
    assert_eq!(value["processGeneration"], 9);
}

#[test]
fn settings_wire_omits_key_fields_and_nulls_unsupported_host_toggles() {
    let settings = Settings {
        revision: 4,
        process_generation: 9,
        gateway_port: 9042,
        gateway_port_from_env: false,
        proxy_mode: ProxyMode::Auto,
        proxy_url: String::new(),
        proxy_list_direction: ProxyListDirection::Whitelist,
        proxy_list_models: Vec::new(),
        proxy_supported_models: vec![ProxySupportedModel {
            id: "gpt-5.6-luna".into(),
            preferred_protocol: "responses".into(),
            zen_free: false,
        }],
        opencode_invite_url: String::new(),
        client_root_url: String::new(),
        client_root_url_from_env: false,
        auto_start: None,
        auto_start_supported: false,
        show_dock_icon: None,
        dock_visibility_supported: false,
        connect_timeout_secs: 30,
        non_stream_timeout_secs: 900,
        stream_idle_timeout_secs: 300,
        routing_mode: RoutingMode::StrictPriority,
        conversation_sticky: false,
    };
    let value = serde_json::to_value(&settings).unwrap();
    let object = value.as_object().unwrap();
    for forbidden in [
        "key",
        "gatewayKey",
        "gateway_key",
        "primaryKey",
        "primary_key",
        "upstreamBaseUrl",
        "upstream_base_url",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "settings must not expose {forbidden}"
        );
    }
    assert_eq!(value["autoStart"], Value::Null);
    assert_eq!(value["showDockIcon"], Value::Null);
    assert_eq!(value["gatewayPortFromEnv"], false);
    assert_eq!(value["autoStartSupported"], false);
    assert_eq!(value["proxyMode"], "auto");
    assert_eq!(value["routingMode"], "strict-priority");
    assert_eq!(
        value["proxySupportedModels"][0]["preferredProtocol"],
        "responses"
    );
}

#[test]
fn settings_update_requires_cas_and_allows_omitted_patch_fields() {
    let parsed: SettingsUpdate = serde_json::from_value(json!({
        "expectedRevision": 7,
        "processGeneration": 9,
        "connectTimeoutSecs": 12
    }))
    .unwrap();
    assert_eq!(parsed.expectation.expected_revision, 7);
    assert_eq!(parsed.expectation.process_generation, 9);
    assert_eq!(parsed.connect_timeout_secs, Some(12));
    assert!(parsed.proxy_mode.is_none());
    assert!(
        serde_json::from_value::<SettingsUpdate>(json!({
            "expectedRevision": 7,
            "processGeneration": 9,
            "gatewayKey": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<SettingsUpdate>(json!({
            "expectedRevision": 7,
            "processGeneration": 9,
            "upstreamBaseUrl": "https://opencode.ai/zen/go"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<SettingsUpdate>(json!({
            "expected_revision": 7,
            "processGeneration": 9
        }))
        .is_err()
    );
}

#[test]
fn proxy_test_dtos_are_camel_case_and_reject_ssrf_and_cas_fields() {
    let parsed: ProxyTestRequest = serde_json::from_value(json!({
        "proxyMode": "direct"
    }))
    .unwrap();
    assert_eq!(parsed.proxy_mode, ProxyMode::Direct);
    assert!(parsed.proxy_url.is_empty());
    assert!(parsed.proxy_list_direction.is_none());

    let with_url: ProxyTestRequest = serde_json::from_value(json!({
        "proxyMode": "manual",
        "proxyUrl": "http://127.0.0.1:7890",
        "proxyListDirection": "blacklist"
    }))
    .unwrap();
    assert_eq!(with_url.proxy_mode, ProxyMode::Manual);
    assert_eq!(with_url.proxy_url, "http://127.0.0.1:7890");
    assert_eq!(
        with_url.proxy_list_direction,
        Some(ProxyListDirection::Blacklist)
    );

    assert!(serde_json::from_value::<ProxyTestRequest>(json!({ "proxy_mode": "direct" })).is_err());
    assert!(serde_json::from_value::<ProxyTestRequest>(json!({ "proxyMode": "Direct" })).is_err());
    assert!(
        serde_json::from_value::<ProxyTestRequest>(json!({
            "proxyMode": "direct",
            "upstreamBaseUrl": "http://127.0.0.1"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ProxyTestRequest>(json!({
            "proxyMode": "direct",
            "expectedRevision": 1
        }))
        .is_err()
    );

    let response = ProxyTestResponse {
        proxy_mode: ProxyMode::Direct,
        status: 401,
        latency_ms: 12,
        revision: 7,
        process_generation: 9,
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(
        value,
        json!({
            "proxyMode": "direct",
            "status": 401,
            "latencyMs": 12,
            "revision": 7,
            "processGeneration": 9
        })
    );
    assert!(value.get("latency_ms").is_none());
    assert!(value.get("upstreamBaseUrl").is_none());
    assert!(value.get("proxyUrl").is_none());
}

#[test]
fn key_mutation_dtos_require_cas_and_reject_secret_fields() {
    let created: KeyCreate = serde_json::from_value(json!({
        "expectedRevision": 4,
        "processGeneration": 9,
        "name": "Laptop"
    }))
    .unwrap();
    assert_eq!(created.expectation.expected_revision, 4);
    assert_eq!(created.expectation.process_generation, 9);
    assert_eq!(created.name, "Laptop");
    assert!(
        serde_json::from_value::<KeyCreate>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "name": "Laptop",
            "value": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<KeyCreate>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "name": "Laptop",
            "key": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<KeyCreate>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "name": "Laptop",
            "gatewayKey": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<KeyCreate>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "name": "Laptop",
            "primaryKey": "ocg-secret"
        }))
        .is_err()
    );

    let patched: KeyUpdate = serde_json::from_value(json!({
        "expectedRevision": 5,
        "processGeneration": 9,
        "enabled": false
    }))
    .unwrap();
    assert_eq!(patched.expectation.expected_revision, 5);
    assert_eq!(patched.enabled, Some(false));
    assert!(patched.name.is_none());
    assert!(
        serde_json::from_value::<KeyUpdate>(json!({
            "expectedRevision": 5,
            "processGeneration": 9,
            "value": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<KeyUpdate>(json!({
            "expected_revision": 5,
            "processGeneration": 9
        }))
        .is_err()
    );
}

#[test]
fn mutation_ack_serializes_without_credential_fields() {
    let ack = MutationAck {
        revision: 8,
        process_generation: 9,
    };
    let value = serde_json::to_value(&ack).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.get("revision"), Some(&json!(8)));
    assert_eq!(object.get("processGeneration"), Some(&json!(9)));
    for forbidden in [
        "key",
        "gatewayKey",
        "gateway_key",
        "primaryKey",
        "primary_key",
        "value",
        "name",
        "id",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "MutationAck must not expose {forbidden}"
        );
    }
}

#[test]
fn account_response_emits_nulls_and_never_carries_secrets() {
    let account = Account {
        id: "acct-1".into(),
        provider_id: "opencode".into(),

        credential_kind: AccountCredentialKind::ApiKey,
        quota_scope: AccountQuotaScope::Key,
        name: "main".into(),
        username: None,
        enabled: true,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
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
        usage_sync_last_success_at: None,
        usage_sync_next_allowed_at: None,
        created_at: "2026-01-31T00:00:00Z".into(),
        updated_at: "2026-01-31T00:00:00Z".into(),
        revision: 4,
        process_generation: 9,
        verification_status: AccountVerificationStatus::NotRequired,
        connection_verified_at: None,
        verification_error: None,
        plan_routable: true,
        custom_config: None,
        model_capabilities: Vec::new(),
        ollama_billing_tier: None,
    };
    let value = serde_json::to_value(&account).unwrap();
    let object = value.as_object().unwrap();
    for forbidden in [
        "key",
        "password",
        "passwordCipher",
        "keyCipher",
        "gatewayKey",
        "gateway_key",
        "primaryKey",
        "referralCode",
        "referral_code",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "Account must not expose {forbidden}"
        );
    }
    assert_eq!(value["username"], Value::Null);
    assert_eq!(value["notes"], Value::Null);
    assert_eq!(value["customConfig"], Value::Null);
    assert_eq!(value["cooldown5hUntil"], Value::Null);
    assert_eq!(value["verificationStatus"], "not_required");
    assert_eq!(value["quotaScope"], "key");
    assert_eq!(value["processGeneration"], 9);

    let listed = AccountList {
        accounts: vec![account.clone()],
        revision: 4,
        process_generation: 9,
    };
    let listed_value = serde_json::to_value(&listed).unwrap();
    assert_eq!(listed_value["accounts"][0]["id"], "acct-1");
    assert_eq!(listed_value["revision"], 4);

    let deleted = AccountMutation {
        account: None,
        revision: 5,
        process_generation: 9,
    };
    let deleted_value = serde_json::to_value(&deleted).unwrap();
    assert_eq!(deleted_value["account"], Value::Null);
    assert_eq!(deleted_value["revision"], 5);
}

#[test]
fn account_requests_accept_write_only_secrets_and_reject_unknown_fields() {
    let created: AccountCreate = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "name": "Go",
        "key": "sk-secret",
        "password": "pw-secret",
        "referralCode": "ref-1"
    }))
    .unwrap();
    assert_eq!(created.expectation.expected_revision, 3);
    assert_eq!(created.key, "sk-secret");
    assert_eq!(created.password.as_deref(), Some("pw-secret"));
    assert_eq!(created.referral_code.as_deref(), Some("ref-1"));
    assert!(created.provider_id.is_none());
    assert!(created.custom_config.is_none());
    assert!(
        serde_json::from_value::<AccountCreate>(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "name": "Go",
            "key": "sk-secret",
            "gatewayKey": "ocg-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountUpdate>(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "providerId": "opencode"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountManagedCreate>(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "name": "draft",
            "key": "sk-secret"
        }))
        .is_err()
    );

    let patched: AccountUpdate = serde_json::from_value(json!({
        "expectedRevision": 4,
        "processGeneration": 9,
        "enabled": false
    }))
    .unwrap();
    assert_eq!(patched.enabled, Some(false));
    assert!(patched.key.is_none());
    assert!(patched.name.is_none());

    let verify: AccountVerify = serde_json::from_value(json!({
        "expectedRevision": 4,
        "processGeneration": 9
    }))
    .unwrap();
    assert_eq!(verify.expectation.expected_revision, 4);
    assert_eq!(verify.expectation.process_generation, 9);
    assert!(
        serde_json::from_value::<AccountVerify>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "key": "sk-secret"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountVerify>(json!({
            "processGeneration": 9
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountVerify>(json!({
            "expected_revision": 4,
            "processGeneration": 9
        }))
        .is_err()
    );

    let verify: AccountManagedKeyVerify = serde_json::from_value(json!({
        "expectedRevision": 5,
        "processGeneration": 9,
        "key": "sk-secret"
    }))
    .unwrap();
    assert_eq!(verify.expectation.expected_revision, 5);
    assert_eq!(verify.expectation.process_generation, 9);
    assert_eq!(verify.key, "sk-secret");
    assert!(
        serde_json::from_value::<AccountManagedKeyVerify>(json!({
            "expectedRevision": 5,
            "processGeneration": 9,
            "key": "sk-secret",
            "setupStep": "key_verification"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountManagedKeyVerify>(json!({
            "expectedRevision": 5,
            "processGeneration": 9
        }))
        .is_err()
    );
}

#[test]
fn custom_capability_writes_accept_only_canonical_or_legacy_shapes() {
    let canonical: AccountModelCapabilityWrite = serde_json::from_value(json!({
        "publicModel": "deepseek-v4-flash",
        "upstreamModel": "deepseek-v4-flash:0731",
        "protocol": "chat_completions"
    }))
    .unwrap();
    assert!(matches!(
        canonical,
        AccountModelCapabilityWrite::Canonical(_)
    ));
    let legacy: AccountModelCapabilityWrite = serde_json::from_value(json!({
        "modelId": "legacy/model",
        "protocol": "chat_completions"
    }))
    .unwrap();
    assert!(matches!(legacy, AccountModelCapabilityWrite::Legacy(_)));

    for malformed in [
        json!({
            "publicModel": "public-only",
            "protocol": "chat_completions"
        }),
        json!({
            "upstreamModel": "upstream-only",
            "protocol": "chat_completions"
        }),
        json!({
            "modelId": "legacy",
            "publicModel": "mixed",
            "upstreamModel": "mixed-upstream",
            "protocol": "chat_completions"
        }),
    ] {
        assert!(
            serde_json::from_value::<AccountModelCapabilityWrite>(malformed).is_err(),
            "malformed capability shape must fail closed"
        );
    }
}

#[test]
fn service_unavailable_and_not_implemented_errors_emit_stable_code_and_cas_tokens() {
    for (label, error, code) in [
        (
            "service_unavailable",
            V3Error::service_unavailable("browser stop failed", 11, 9),
            ERROR_SERVICE_UNAVAILABLE,
        ),
        (
            "not_implemented",
            V3Error::not_implemented("protocol probes are not available", 11, 9),
            ERROR_NOT_IMPLEMENTED,
        ),
    ] {
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], code, "{label}");
        assert_eq!(value["currentRevision"], 11, "{label}");
        assert_eq!(value["processGeneration"], 9, "{label}");
    }
}

#[test]
fn browser_dtos_are_distinct_secret_free_and_emit_required_nulls() {
    let capabilities = BrowserCapabilities {
        mode: BrowserMode::Unsupported,
        reason: None,
        revision: 11,
        process_generation: 9,
    };
    let capabilities_value = serde_json::to_value(&capabilities).unwrap();
    assert_eq!(capabilities_value["mode"], "unsupported");
    assert_eq!(capabilities_value["reason"], Value::Null);
    assert_eq!(capabilities_value["revision"], 11);
    assert_eq!(capabilities_value["processGeneration"], 9);
    assert!(capabilities_value.get("workerUrl").is_none());
    assert!(capabilities_value.get("controlToken").is_none());

    let native = BrowserOpen {
        mode: BrowserMode::Native,
        session_token: None,
        revision: 4,
        process_generation: 9,
    };
    let native_value = serde_json::to_value(&native).unwrap();
    assert_eq!(native_value["mode"], "native");
    assert_eq!(native_value["sessionToken"], Value::Null);
    assert_eq!(native_value.as_object().unwrap().len(), 4);

    let remote = BrowserOpen {
        mode: BrowserMode::Remote,
        session_token: Some("opaque-session".into()),
        revision: 4,
        process_generation: 9,
    };
    let remote_value = serde_json::to_value(&remote).unwrap();
    assert_eq!(remote_value["mode"], "remote");
    assert_eq!(remote_value["sessionToken"], "opaque-session");
    assert!(remote_value.get("workerUrl").is_none());
    assert!(remote_value.get("vncWsUrl").is_none());
    assert!(remote_value.get("controlToken").is_none());
    assert_eq!(remote_value.as_object().unwrap().len(), 4);

    let parsed: BrowserOpenRequest = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "target": "google_signup" }))
    .unwrap();
    assert_eq!(parsed.expectation.expected_revision, 3);
    assert_eq!(parsed.target, BrowserTarget::GoogleSignup);
    assert!(
        serde_json::from_value::<BrowserOpenRequest>(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "target": "console",
            "workerUrl": "http://browser/session"
        }))
        .is_err()
    );
}

const ACCOUNTS_CATALOG_PREFIX: &[&str] = &[
    "ControlRevision",
    "MutationAck",
    "MutationExpectation",
    "PricingRevision",
    "V3Error",
    "ConnectionInfo",
    "ConnectionSubKey",
    "Settings",
    "SettingsUpdate",
    "ProxySupportedModel",
    "KeyCreate",
    "KeyUpdate",
    "Account",
    "AccountList",
    "AccountMutation",
    "AccountCustomConfig",
    "AccountModelCapability",
    "AccountCreate",
    "AccountManagedCreate",
    "AccountModelTestRequest",
    "AccountModelTestResponse",
    "AccountUpdate",
    "AccountOrder",
    "AccountSetupUpdate",
    "AccountCustomConfigUpdate",
    "AccountCustomConfigWrite",
    "AccountModelCapabilitiesUpdate",
    "AccountModelCapabilityWrite",
];

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

fn schema_allows_null(schema: &Value) -> bool {
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if schema
        .get("type")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|value| value == "null"))
    {
        return true;
    }
    schema
        .get("anyOf")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(schema_allows_null))
}

fn assert_secret_free(value: &Value) {
    for name in json_field_names(value) {
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
            "provider DTO leaked field {name}: {value}"
        );
    }
    for text in json_string_values(value) {
        assert!(
            !text.contains("sk-secret")
                && !text.contains("ocg-secret")
                && !text.contains("pw-secret")
                && !text.contains("user:pass@"),
            "provider DTO leaked secret sample {text}: {value}"
        );
    }
}

fn chat_evidence() -> EffectiveProtocolEvidence {
    EffectiveProtocolEvidence {
        protocol: AccountUpstreamProtocol::ChatCompletions,
        available: true,
        enabled: true,
        source: ContractEvidenceSource::Static,
        verified_at: None,
        observed_at: None,
        last_probe_result: None,
        last_probe_at: None,
        last_probe_error: None,
        r#override: ProtocolOverrideState::Auto,
    }
}

fn sample_catalog_entry() -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        provider_id: "opencode".into(),

        origin: ProviderOrigin::Builtin,
        editable: false,
        deletable: false,
        offering: "plan".into(),
        display_name: "OpenCode Go".into(),
        display_family: "OpenCode".into(),
        credential_kind: AccountCredentialKind::ApiKey,
        quota_scope: AccountQuotaScope::Key,
        singleton: false,
        creation_availability: "available".into(),
        creation_unavailable_reason: None,
        verification_policy: "not_required".into(),
        verification_runtime_availability: "optional".into(),
        routable: true,
        managed_registration: true,
        pricing_availability: "available".into(),
        usage_availability: "available".into(),
        manual_usage_calibration: false,
        quota_unit: "usd".into(),
        model_source: "builtin_go_protocol_table".into(),
        key_prefix: None,
        auth_schemes: vec![AccountAuthScheme::Bearer],
        upstream_protocols: vec![
            AccountUpstreamProtocol::ChatCompletions,
            AccountUpstreamProtocol::Responses,
            AccountUpstreamProtocol::Messages,
        ],
        form_fields: vec![
            ProviderCatalogFormField {
                id: "name".into(),
                kind: "text".into(),
                required: true,
                immutable_after_create: false,
            },
            ProviderCatalogFormField {
                id: "key".into(),
                kind: "secret".into(),
                required: true,
                immutable_after_create: false,
            },
            ProviderCatalogFormField {
                id: "purchase_date".into(),
                kind: "date".into(),
                required: false,
                immutable_after_create: false,
            },
            ProviderCatalogFormField {
                id: "notes".into(),
                kind: "text".into(),
                required: false,
                immutable_after_create: false,
            },
        ],
        model_aliases: Vec::new(),
    }
}

fn sample_contracts() -> ProviderContracts {
    let goat = ProviderContractGroup {
        scope_kind: ContractScopeKind::Provider,
        scope_id: "command-code".into(),
        provider_id: "command-code".into(),
        static_protocol_snapshot_date: None,
        accounts: vec![ProviderAccountChoice {
            id: "goat-1".into(),
            name: "draft".into(),
            enabled: false,
            verification_status: AccountVerificationStatus::Pending,
        }],
        catalog: EffectiveCatalog {
            source: "static".into(),
            source_url: String::new(),
            refreshed_at: None,
            models: Vec::new(),
            refresh_supported: false,
        },
        models: vec![EffectiveModelContract {
            alias: "deepseek-v4-flash".into(),
            model_id: "deepseek-v4-flash".into(),
            preferred_protocol: AccountUpstreamProtocol::ChatCompletions,
            protocols: EffectiveModelProtocols {
                chat_completions: Some(chat_evidence()),
                responses: None,
                messages: Some(EffectiveProtocolEvidence {
                    protocol: AccountUpstreamProtocol::Messages,
                    available: false,
                    enabled: false,
                    source: ContractEvidenceSource::Preset,
                    verified_at: None,
                    observed_at: None,
                    last_probe_result: None,
                    last_probe_at: None,
                    last_probe_error: None,
                    r#override: ProtocolOverrideState::Auto,
                }),
            },
            routable: false,
            disabled_reasons: vec!["offering is not routable".into()],
        }],
        pricing: CapabilitySummary {
            availability: "unavailable".into(),
        },
        usage: CapabilitySummary {
            availability: "unavailable".into(),
        },
        card: CardCapabilitySummary {
            fetch_zen_models: false,
            discover_models: false,
            protocol_probe: false,
            catalog_refresh: false,
        },
        catalog_routable: false,
        production_inference: false,
        disabled_reasons: vec!["offering is not routable".into()],
        revision: 2,
    };
    ProviderContracts {
        providers: vec![goat],
        custom_endpoints: vec![CustomEndpointContract {
            scope_kind: ContractScopeKind::CustomEndpoint,
            scope_id: "custom-1".into(),
            provider_id: "custom".into(),
            account: ProviderAccountChoice {
                id: "custom-1".into(),
                name: "lan".into(),
                enabled: false,
                verification_status: AccountVerificationStatus::Pending,
            },
            catalog: EffectiveCatalog {
                source: "account_declared".into(),
                source_url: String::new(),
                refreshed_at: None,
                models: vec!["local-model".into()],
                refresh_supported: true,
            },
            models: Vec::new(),
            pricing: CapabilitySummary {
                availability: "unpriced".into(),
            },
            usage: CapabilitySummary {
                availability: "unavailable".into(),
            },
            card: CardCapabilitySummary {
                fetch_zen_models: false,
                discover_models: true,
                protocol_probe: true,
                catalog_refresh: true,
            },
            catalog_routable: true,
            production_inference: false,
            disabled_reasons: vec!["account is not enabled".into()],
            revision: 8,
        }],
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    }
}

#[test]
fn account_schema_keeps_required_fields() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    assert_eq!(
        defs["Account"]["required"],
        json!([
            "id",
            "providerId",
            "credentialKind",
            "quotaScope",
            "name",
            "username",
            "enabled",
            "accountType",
            "setupStep",
            "purchaseDate",
            "expiresOn",
            "cooldownUntil",
            "cooldownGenericUntil",
            "cooldown5hUntil",
            "cooldownWeekUntil",
            "cooldownMonthUntil",
            "cooldownFreeUntil",
            "lastError",
            "authError",
            "notes",
            "usageSyncLastSuccessAt",
            "usageSyncNextAllowedAt",
            "createdAt",
            "updatedAt",
            "revision",
            "processGeneration",
            "verificationStatus",
            "connectionVerifiedAt",
            "verificationError",
            "planRoutable",
            "customConfig",
            "modelCapabilities",
            "ollamaBillingTier"
        ])
    );
}

#[test]
fn provider_catalog_emits_nulls_camel_case_and_no_secrets() {
    let catalog = ProviderCatalog {
        entries: vec![sample_catalog_entry()],
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let value = serde_json::to_value(&catalog).unwrap();
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["pricingRevision"], "seed");
    assert_eq!(value["entries"][0]["providerId"], "opencode");
    assert_eq!(value["entries"][0]["verificationPolicy"], "not_required");
    assert_eq!(
        value["entries"][0]["verificationRuntimeAvailability"],
        "optional"
    );
    assert_eq!(value["entries"][0]["routable"], true);
    assert_eq!(
        value["entries"][0]["upstreamProtocols"][0],
        "chat_completions"
    );
    assert!(value.get("modelCapabilities").is_none());
    assert!(value.get("creation_unavailable_reason").is_none());
    assert_secret_free(&value);

    let schema = contract_schema();
    let required = schema["$defs"]["ProviderCatalogEntry"]["required"]
        .as_array()
        .unwrap();
    for field in ["creationUnavailableReason", "keyPrefix"] {
        assert!(
            required.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
}

#[test]
fn zen_and_contract_responses_keep_cas_distinct_from_display_revisions() {
    let settings = ZenFreeSettings {
        account_id: "zen-free".into(),
        enabled: true,
        revision: 12,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let settings_value = serde_json::to_value(&settings).unwrap();
    assert_eq!(
        settings_value,
        json!({
            "accountId": "zen-free",
            "enabled": true,
            "revision": 12,
            "processGeneration": 9,
            "pricingRevision": "seed"
        })
    );
    assert_secret_free(&settings_value);

    let models = ZenFreeModels {
        account_id: "zen-free".into(),
        models: vec![ZenFreeModel {
            model_id: "hy3-free".into(),
            alias: "hy3".into(),
        }],
        refreshed_at: None,
        source_url: "https://opencode.ai/zen/v1/models".into(),
        revision: 12,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let models_value = serde_json::to_value(&models).unwrap();
    assert_eq!(models_value["refreshedAt"], Value::Null);
    assert_eq!(models_value["accountId"], "zen-free");
    assert_eq!(models_value["pricingRevision"], "seed");
    assert_secret_free(&models_value);

    let contracts = sample_contracts();
    let contracts_value = serde_json::to_value(&contracts).unwrap();
    assert_eq!(contracts_value["revision"], 11);
    assert_eq!(contracts_value["processGeneration"], 9);
    assert_eq!(contracts_value["providers"][0]["revision"], 2);
    assert_eq!(contracts_value["providers"][0]["scopeKind"], "provider");
    assert_eq!(
        contracts_value["customEndpoints"][0]["scopeKind"],
        "custom_endpoint"
    );
    assert_eq!(
        contracts_value["providers"][0]["accounts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        contracts_value["providers"][0]["models"][0]["protocols"]["responses"],
        Value::Null
    );
    assert_eq!(
        contracts_value["providers"][0]["models"][0]["protocols"]["chat_completions"]["lastProbeError"],
        Value::Null
    );
    assert!(
        contracts_value["providers"][0]
            .as_object()
            .unwrap()
            .get("scopeRevision")
            .is_none()
    );
    assert_eq!(
        contracts_value["providers"][0]["accounts"][0]["verificationStatus"],
        "pending"
    );
    assert_eq!(
        contracts_value["providers"][0]["card"]["protocolProbe"],
        false
    );
    assert_eq!(
        contracts_value["customEndpoints"][0]["catalog"]["refreshedAt"],
        Value::Null
    );
    assert!(contracts_value.get("expectedRevision").is_none());
    assert_secret_free(&contracts_value);

    let schema = contract_schema();
    let override_state = schema["$defs"]["ProtocolOverrideState"]
        .as_object()
        .unwrap();
    assert_eq!(
        override_state["enum"],
        json!(["auto", "force_on", "force_off"])
    );
    let protocol_required = schema["$defs"]["EffectiveModelProtocols"]["required"]
        .as_array()
        .unwrap();
    for field in ["chat_completions", "responses", "messages"] {
        assert!(
            protocol_required.iter().any(|value| value == field),
            "{field} must stay required so missing protocols emit null"
        );
    }
    assert!(schema["$defs"]["ProviderContractGroup"]["properties"]["revision"].is_object());
    assert!(
        schema["$defs"]["ProviderContractGroup"]["properties"]
            .as_object()
            .unwrap()
            .get("scopeRevision")
            .is_none()
    );
}

#[test]
fn provider_mutation_requests_require_cas_allow_omission_and_reject_unknown_fields() {
    let zen: ZenFreeSettingsUpdate = serde_json::from_value(json!({
        "expectedRevision": 4,
        "processGeneration": 9,
        "enabled": true
    }))
    .unwrap();
    assert_eq!(zen.expectation.expected_revision, 4);
    assert!(zen.enabled);
    assert!(
        serde_json::from_value::<ZenFreeSettingsUpdate>(json!({
            "expectedRevision": 4,
            "processGeneration": 9,
            "enabled": true,
            "key": "sk-secret"
        }))
        .is_err()
    );

    let overrides: ModelProtocolOverridesUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "overrides": [
            { "modelId": "glm-5.2", "protocol": "chat_completions", "state": "force_on" }
        ]
    }))
    .unwrap();
    assert_eq!(overrides.overrides.len(), 1);
    assert_eq!(overrides.overrides[0].state, ProtocolOverrideState::ForceOn);
    assert!(
        serde_json::from_value::<ModelProtocolOverridesUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "overrides": [
                { "modelId": "glm-5.2", "protocol": "chat_completions", "state": "force_on" }
            ],
            "scopeRevision": 2
        }))
        .is_err()
    );

    assert!(
        serde_json::from_value::<ProtocolProbeRequest>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "modelId": "gpt-5.6-luna"
        }))
        .is_err()
    );
    let with_account: ProtocolProbeRequest = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "accountId": "acct-1",
        "modelId": "gpt-5.6-luna",
        "protocols": ["chat_completions", "responses"]
    }))
    .unwrap();
    assert_eq!(with_account.account_id.as_deref(), Some("acct-1"));
    assert_eq!(
        with_account.protocols,
        vec![
            AccountUpstreamProtocol::ChatCompletions,
            AccountUpstreamProtocol::Responses
        ]
    );
    assert!(
        serde_json::from_value::<ProtocolProbeRequest>(json!({
            "expected_revision": 11,
            "processGeneration": 9,
            "modelId": "gpt-5.6-luna"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ProtocolProbeRequest>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "modelId": "gpt-5.6-luna",
            "key": "sk-secret"
        }))
        .is_err()
    );

    let probe = ProtocolProbeResponse {
        account_id: None,
        provider_id: "custom".into(),
        model_id: "gpt-5.6-luna".into(),
        results: vec![ProtocolProbeResult {
            protocol: AccountUpstreamProtocol::ChatCompletions,
            success: false,
            skipped: true,
            error: None,
        }],
        contract: None,
        revision: 12,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let probe_value = serde_json::to_value(&probe).unwrap();
    assert_eq!(probe_value["accountId"], Value::Null);
    assert_eq!(probe_value["providerId"], "custom");
    assert_eq!(probe_value["contract"], Value::Null);
    assert_eq!(probe_value["pricingRevision"], "seed");
    assert_eq!(probe_value["results"][0]["protocol"], "chat_completions");
    assert_eq!(probe_value["results"][0]["error"], Value::Null);
    assert_secret_free(&probe_value);

    let schema = contract_schema();
    let probe_request = &schema["$defs"]["ProtocolProbeRequest"];
    assert_eq!(probe_request["additionalProperties"], false);
    let required = probe_request["required"].as_array().unwrap();
    assert!(required.iter().any(|value| value == "expectedRevision"));
    assert!(required.iter().any(|value| value == "processGeneration"));
    assert!(required.iter().any(|value| value == "modelId"));
    assert!(!required.iter().any(|value| value == "accountId"));
    assert!(required.iter().any(|value| value == "protocols"));
    let response_required = schema["$defs"]["ProtocolProbeResponse"]["required"]
        .as_array()
        .unwrap();
    assert!(response_required.iter().any(|value| value == "accountId"));
    assert!(response_required.iter().any(|value| value == "providerId"));
    assert!(response_required.iter().any(|value| value == "contract"));
    assert!(
        response_required
            .iter()
            .any(|value| value == "pricingRevision")
    );
}

#[test]
fn custom_model_discovery_is_an_operational_probe_without_cas() {
    let request: CustomModelDiscoveryRequest = serde_json::from_value(json!({
        "endpointUrl": "https://api.example.com/v1/chat/completions",
        "upstreamProtocol": "chat_completions",
        "apiKey": "sk-secret"
    }))
    .unwrap();
    assert_eq!(
        request.endpoint_url,
        "https://api.example.com/v1/chat/completions"
    );
    assert_eq!(
        request.upstream_protocol,
        AccountUpstreamProtocol::ChatCompletions
    );
    assert_eq!(request.api_key.as_deref(), Some("sk-secret"));
    assert!(request.account_id.is_none());

    let with_account: CustomModelDiscoveryRequest = serde_json::from_value(json!({
        "endpointUrl": "http://127.0.0.1:9/v1/messages",
        "upstreamProtocol": "messages",
        "accountId": "acct-1"
    }))
    .unwrap();
    assert_eq!(with_account.account_id.as_deref(), Some("acct-1"));
    assert!(with_account.api_key.is_none());

    assert!(
        serde_json::from_value::<CustomModelDiscoveryRequest>(json!({
            "endpointUrl": "https://api.example.com/v1/chat/completions",
            "upstreamProtocol": "chat_completions",
            "expectedRevision": 11
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<CustomModelDiscoveryRequest>(json!({
            "endpoint_url": "https://api.example.com/v1/chat/completions",
            "upstreamProtocol": "chat_completions"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<CustomModelDiscoveryRequest>(json!({
            "endpointUrl": "https://api.example.com/v1/chat/completions",
            "upstreamProtocol": "chat_completions",
            "key": "sk-secret"
        }))
        .is_err()
    );

    let response = CustomModelDiscoveryResponse {
        models: vec!["org/model".into()],
        truncated: false,
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["models"][0], "org/model");
    assert_eq!(value["truncated"], false);
    assert_eq!(value["revision"], 11);
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["pricingRevision"], "seed");
    assert!(value.get("apiKey").is_none());
    assert!(value.get("api_key").is_none());
    assert!(value.get("endpointUrl").is_none());
    assert_secret_free(&value);

    let schema = contract_schema();
    let request_schema = &schema["$defs"]["CustomModelDiscoveryRequest"];
    assert_eq!(request_schema["additionalProperties"], false);
    let required = request_schema["required"].as_array().unwrap();
    assert!(required.iter().any(|value| value == "endpointUrl"));
    assert!(required.iter().any(|value| value == "upstreamProtocol"));
    assert!(!required.iter().any(|value| value == "apiKey"));
    assert!(!required.iter().any(|value| value == "accountId"));
    assert!(!required.iter().any(|value| value == "expectedRevision"));
    assert!(!required.iter().any(|value| value == "processGeneration"));
    let response_required = schema["$defs"]["CustomModelDiscoveryResponse"]["required"]
        .as_array()
        .unwrap();
    for field in [
        "models",
        "truncated",
        "revision",
        "processGeneration",
        "pricingRevision",
    ] {
        assert!(
            response_required.iter().any(|value| value == field),
            "{field} must stay required"
        );
    }
    assert!(
        schema["$defs"]["CustomModelDiscoveryResponse"]["properties"]["truncated"]["type"]
            == "boolean"
    );
}

#[test]
fn updater_dtos_use_camel_case_nulls_and_install_requires_cas() {
    let check = UpdateCheck {
        current_version: "1.0.0".into(),
        latest_version: "1.1.0".into(),
        update_available: true,
        release_url: "https://github.com/klarkxy/open-console-gateway/releases/latest".into(),
        install_supported: false,
        revision: 11,
        process_generation: 9,
    };
    let check_value = serde_json::to_value(&check).unwrap();
    assert_eq!(
        check_value,
        json!({
            "currentVersion": "1.0.0",
            "latestVersion": "1.1.0",
            "updateAvailable": true,
            "releaseUrl": "https://github.com/klarkxy/open-console-gateway/releases/latest",
            "installSupported": false,
            "revision": 11,
            "processGeneration": 9 })
    );
    assert!(check_value.get("current_version").is_none());
    assert!(check_value.get("release_url").is_none());
    assert_secret_free(&check_value);

    let idle = DesktopUpdate {
        phase: DesktopUpdatePhase::Idle,
        downloaded: 0,
        total: None,
        error: None,
        current_version: "1.0.0".into(),
        install_supported: true,
        revision: 11,
        process_generation: 9,
    };
    let idle_value = serde_json::to_value(&idle).unwrap();
    assert_eq!(
        idle_value,
        json!({
            "phase": "idle",
            "downloaded": 0,
            "total": null,
            "error": null,
            "currentVersion": "1.0.0",
            "installSupported": true,
            "revision": 11,
            "processGeneration": 9 })
    );
    assert!(idle_value.get("current_version").is_none());
    assert!(idle_value.get("install_supported").is_none());
    assert_secret_free(&idle_value);

    let failed = DesktopUpdate {
        phase: DesktopUpdatePhase::Failed,
        downloaded: 64,
        total: Some(128),
        error: Some("signature verification failed".into()),
        ..idle
    };
    let failed_value = serde_json::to_value(&failed).unwrap();
    assert_eq!(failed_value["phase"], "failed");
    assert_eq!(failed_value["downloaded"], 64);
    assert_eq!(failed_value["total"], 128);
    assert_eq!(failed_value["error"], "signature verification failed");

    let install: InstallUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "expectedVersion": "1.1.0"
    }))
    .unwrap();
    assert_eq!(install.expectation.expected_revision, 11);
    assert_eq!(install.expectation.process_generation, 9);
    assert_eq!(install.expected_version, "1.1.0");
    assert!(
        serde_json::from_value::<InstallUpdate>(json!({
            "processGeneration": 9,
            "expectedVersion": "1.1.0"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<InstallUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expected_version": "1.1.0"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<InstallUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedVersion": "1.1.0",
            "key": "sk-secret"
        }))
        .is_err()
    );

    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in UPDATER_CATALOG_TYPES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }

    let check_required = defs["UpdateCheck"]["required"].as_array().unwrap();
    assert_eq!(
        check_required,
        &vec![
            json!("currentVersion"),
            json!("latestVersion"),
            json!("updateAvailable"),
            json!("releaseUrl"),
            json!("installSupported"),
            json!("revision"),
            json!("processGeneration"),
        ]
    );
    assert!(
        !defs["UpdateCheck"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("expectedRevision")
    );

    let status_required = defs["DesktopUpdate"]["required"].as_array().unwrap();
    for field in [
        "phase",
        "downloaded",
        "total",
        "error",
        "currentVersion",
        "installSupported",
        "revision",
        "processGeneration",
    ] {
        assert!(
            status_required.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
    let status_props = defs["DesktopUpdate"]["properties"].as_object().unwrap();
    assert!(schema_allows_null(&status_props["total"]));
    assert!(schema_allows_null(&status_props["error"]));
    assert!(!schema_allows_null(&status_props["downloaded"]));
    assert!(!status_props.contains_key("current_version"));
    assert!(!status_props.contains_key("install_supported"));

    let install_schema = &defs["InstallUpdate"];
    assert_eq!(install_schema["additionalProperties"], false);
    let install_required = install_schema["required"].as_array().unwrap();
    assert_eq!(
        install_required,
        &vec![
            json!("expectedRevision"),
            json!("processGeneration"),
            json!("expectedVersion"),
        ]
    );
    assert!(
        !install_schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("expected_version")
    );
    assert_eq!(
        defs["DesktopUpdatePhase"]["enum"],
        json!(["idle", "checking", "downloading", "installing", "failed"])
    );
}

const PROVIDER_CATALOG_TYPES: &[&str] = &[
    "ProviderCatalog",
    "ProviderCatalogEntry",
    "ProviderCatalogFormField",
    "ProviderModelCapability",
    "ZenFreeSettings",
    "ZenFreeSettingsUpdate",
    "ZenFreeModels",
    "ZenFreeModel",
    "ProviderContracts",
    "ProviderContractGroup",
    "CustomEndpointContract",
    "ProviderAccountChoice",
    "EffectiveCatalog",
    "EffectiveModelContract",
    "EffectiveModelProtocols",
    "EffectiveProtocolEvidence",
    "CapabilitySummary",
    "CardCapabilitySummary",
    "ModelProtocolOverridesUpdate",
    "ModelProtocolOverride",
    "ProtocolOverrideState",
    "ProtocolProbeRequest",
    "ProtocolProbeResult",
    "ProtocolProbeResponse",
];

const PRICING_CATALOG_TYPES: &[&str] = &[
    "PricingSnapshot",
    "PricingLimits",
    "PricingModel",
    "PricingAdjustment",
    "PricingTimeWindow",
    "PricingRefresh",
    "PricingRefreshStatus",
    "PricingMultiplierChange",
    "PricingRefreshUpdate",
    "PricingRefreshPolicy",
    "PricingMultipliersUpdate",
    "PricingMultiplierWrite",
    "ProviderPricing",
    "PricingAvailability",
];

const USAGE_CATALOG_TYPES: &[&str] = &[
    "UsageWindow",
    "UsageMutation",
    "AccountUsageUpdate",
    "ProviderUsage",
    "QuotaWindow",
    "CreditBalance",
    "UsageSyncState",
    "UsageAvailability",
];
const AUTH_CATALOG_TYPES: &[&str] = &["AuthStatus", "AuthRegister", "AuthLogin", "AuthLogout"];

const BROWSER_CATALOG_TYPES: &[&str] = &[
    "BrowserMode",
    "BrowserTarget",
    "BrowserCapabilities",
    "BrowserOpenRequest",
    "BrowserOpen",
];

fn sample_pricing_snapshot() -> PricingSnapshot {
    PricingSnapshot {
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed-2026-08-16-local-v4".into(),
        activated_at: "2026-08-16T12:00:00.000Z".into(),
        document_updated_at: "2026-08-16T00:00:00.000Z".into(),
        source_url: "https://opencode.ai/docs/go/".into(),
        content_hash: "embedded-opencode-go-2026-08-16".into(),
        adjustment_policy_version: "local-v4".into(),
        limits: PricingLimits {
            window_5h: 12.0,
            window_week: 30.0,
            window_month: 60.0,
        },
        models: vec![PricingModel {
            model_id: "grok-4.5".into(),
            display_name: "Grok 4.5".into(),
            input: 2.0,
            output: 6.0,
            cache_read: 0.3,
            cache_write: None,
            usage: 15.0,
            quota_multiplier: 4.0,
            min_input_tokens: None,
            max_input_tokens: None,
            time_window: PricingTimeWindow::Always,
            adjustments: vec![PricingAdjustment {
                label: "highspeed alias".into(),
                multiplier: 2.0,
                applies_to: "input,output".into(),
            }],
        }],
    }
}

const OBSERVABILITY_CATALOG_TYPES: &[&str] = &[
    "GatewayStatus",
    "ApplicationModels",
    "DashboardSummary",
    "DailyModelTokens",
    "DailyTokensByModel",
    "GatewayLog",
    "GatewayLogs",
    "ForwardLog",
    "ForwardLogSummary",
    "ForwardLogs",
    "ForwardLogClientKey",
    "ForwardLogKeys",
    "ForwardLogModels",
    "GatewayLogQuery",
    "ForwardLogQuery",
    "DailyTokensQuery",
];

const PROXY_TEST_CATALOG_TYPES: &[&str] = &["ProxyTestRequest", "ProxyTestResponse"];
const MANAGED_KEY_VERIFY_CATALOG_TYPES: &[&str] = &["AccountManagedKeyVerify"];

const CUSTOM_DISCOVERY_CATALOG_TYPES: &[&str] = &[
    "CustomModelDiscoveryRequest",
    "CustomModelDiscoveryResponse",
];
const CLAUDE_DESKTOP_CATALOG_TYPES: &[&str] = &["ClaudeDesktopModels", "ClaudeDesktopModelsUpdate"];

const UPDATER_CATALOG_TYPES: &[&str] = &["UpdateCheck", "DesktopUpdate", "InstallUpdate"];
const USAGE_REFRESH_CATALOG_TYPES: &[&str] = &[
    "UsageRefresh",
    "UsageRefreshUpdate",
    "UsageRefreshThrottleError",
];
const PROVIDER_REFRESH_CATALOG_TYPES: &[&str] = &[
    "ProviderModelsRefreshUpdate",
    "ProviderModels",
    "ProviderPricingSnapshot",
    "ProviderPricingValue",
    "ProviderPricingRefresh",
    "ProviderPricingRefreshUpdate",
];
const ACCOUNT_TRANSFER_CATALOG_TYPES: &[&str] = &[
    "AccountExportRequest",
    "AccountExport",
    "AccountImportPreviewRequest",
    "AccountImportPreview",
    "AccountImportPreviewItem",
    "AccountImportDisposition",
    "AccountImportRequest",
    "AccountImportResult",
];
const CPA_CATALOG_TYPES: &[&str] = &[
    "CpaIntegration",
    "CpaIntegrationUpdate",
    "CpaTestRequest",
    "CpaConnectionReport",
    "CpaModel",
    "CpaModels",
    "CpaAccounts",
    "CpaAccount",
    "CpaAccountStatusUpdate",
    "CpaAccountDelete",
    "CpaQuotaReset",
    "CpaOAuthProvider",
    "CpaOAuthStartRequest",
    "CpaOAuthStart",
    "CpaOAuthStatus",
    "CpaOAuthSessionDelete",
    "CpaCliImportSource",
    "CpaCliImports",
    "CpaCliImportRequest",
    "CpaCliImportResult",
    "CpaRuntime",
    "CpaRuntimePhase",
    "CpaRuntimeCheck",
    "CpaRuntimeInstall",
    "CpaRuntimeLogs",
    "CpaRuntimeKey",
    "CpaRuntimeKeys",
    "CpaRuntimeKeyCreated",
];
const DYNAMIC_PROVIDER_CATALOG_TYPES: &[&str] = &[
    "ProviderDefinitionAuthKind",
    "ProviderDefinitionModel",
    "ProviderDefinition",
    "ProviderDefinitionCreate",
    "ProviderDefinitionUpdate",
    "ProviderDefinitionMutation",
    "ProviderDefinitionDiscoverRequest",
    "ProviderDefinitionDiscoverResponse",
    "ProviderDefinitionTestRequest",
    "ProviderDefinitionTestResponse",
];
const OLLAMA_USAGE_CATALOG_TYPES: &[&str] = &["OllamaBillingTier"];

#[test]
fn catalog_type_names_append_pricing_dtos_after_the_provider_prefix() {
    let prefix_len = ACCOUNTS_CATALOG_PREFIX.len() + PROVIDER_CATALOG_TYPES.len();
    let pricing_end = prefix_len + PRICING_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[..ACCOUNTS_CATALOG_PREFIX.len()],
        ACCOUNTS_CATALOG_PREFIX
    );
    assert_eq!(
        &CATALOG_TYPE_NAMES[ACCOUNTS_CATALOG_PREFIX.len()..prefix_len],
        PROVIDER_CATALOG_TYPES
    );
    assert_eq!(
        &CATALOG_TYPE_NAMES[prefix_len..pricing_end],
        PRICING_CATALOG_TYPES
    );
    let observability_end = pricing_end + OBSERVABILITY_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[pricing_end..observability_end],
        OBSERVABILITY_CATALOG_TYPES
    );
    let usage_end = observability_end + USAGE_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[observability_end..usage_end],
        USAGE_CATALOG_TYPES
    );
    let auth_end = usage_end + AUTH_CATALOG_TYPES.len();
    assert_eq!(&CATALOG_TYPE_NAMES[usage_end..auth_end], AUTH_CATALOG_TYPES);
    let proxy_end = auth_end + PROXY_TEST_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[auth_end..proxy_end],
        PROXY_TEST_CATALOG_TYPES
    );
    let custom_discovery_end = proxy_end + CUSTOM_DISCOVERY_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[proxy_end..custom_discovery_end],
        CUSTOM_DISCOVERY_CATALOG_TYPES
    );
    let claude_end = custom_discovery_end + CLAUDE_DESKTOP_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[custom_discovery_end..claude_end],
        CLAUDE_DESKTOP_CATALOG_TYPES
    );
    let account_verify_end = claude_end + 1;
    assert_eq!(
        &CATALOG_TYPE_NAMES[claude_end..account_verify_end],
        ["AccountVerify"]
    );
    let browser_end = account_verify_end + BROWSER_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[account_verify_end..browser_end],
        BROWSER_CATALOG_TYPES
    );
    let updater_end = browser_end + UPDATER_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[browser_end..updater_end],
        UPDATER_CATALOG_TYPES
    );
    let managed_end = updater_end + MANAGED_KEY_VERIFY_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[updater_end..managed_end],
        MANAGED_KEY_VERIFY_CATALOG_TYPES
    );
    let usage_refresh_end = managed_end + USAGE_REFRESH_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[managed_end..usage_refresh_end],
        USAGE_REFRESH_CATALOG_TYPES
    );
    let provider_refresh_end = usage_refresh_end + PROVIDER_REFRESH_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[usage_refresh_end..provider_refresh_end],
        PROVIDER_REFRESH_CATALOG_TYPES
    );
    let account_transfer_end = provider_refresh_end + ACCOUNT_TRANSFER_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[provider_refresh_end..account_transfer_end],
        ACCOUNT_TRANSFER_CATALOG_TYPES
    );
    let cpa_end = account_transfer_end + CPA_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[account_transfer_end..cpa_end],
        CPA_CATALOG_TYPES
    );
    let dynamic_end = cpa_end + DYNAMIC_PROVIDER_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[cpa_end..dynamic_end],
        DYNAMIC_PROVIDER_CATALOG_TYPES
    );
    let ollama_end = dynamic_end + OLLAMA_USAGE_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[dynamic_end..ollama_end],
        OLLAMA_USAGE_CATALOG_TYPES
    );
    assert_eq!(
        &CATALOG_TYPE_NAMES[ollama_end..],
        &[
            "PlatformAccounts",
            "PlatformAccount",
            "PlatformLink",
            "PlatformSnapshot",
            "PlatformCreate",
            "PlatformUpdate",
            "PlatformLinkWrite",
            "PlatformRefresh",
        ]
    );
}

#[test]
fn pricing_snapshot_uses_cas_revision_and_emits_required_nulls() {
    let snapshot = sample_pricing_snapshot();
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["revision"], 11);
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["pricingRevision"], "seed-2026-08-16-local-v4");
    assert_eq!(value["limits"]["window5h"], 12.0);
    assert_eq!(value["limits"]["windowWeek"], 30.0);
    assert_eq!(value["limits"]["windowMonth"], 60.0);
    assert_eq!(value["models"][0]["modelId"], "grok-4.5");
    assert_eq!(value["models"][0]["cacheWrite"], Value::Null);
    assert_eq!(value["models"][0]["minInputTokens"], Value::Null);
    assert_eq!(value["models"][0]["maxInputTokens"], Value::Null);
    assert_eq!(value["models"][0]["timeWindow"], "always");
    assert_eq!(
        value["models"][0]["adjustments"][0]["appliesTo"],
        "input,output"
    );
    assert!(value.get("snapshotJson").is_none());
    assert!(
        value
            .as_object()
            .unwrap()
            .get("revision")
            .unwrap()
            .is_number()
    );
    assert!(
        serde_json::from_value::<PricingSnapshot>(json!({
            "revision": "seed-2026-08-16-local-v4",
            "processGeneration": 9,
            "pricingRevision": "seed-2026-08-16-local-v4",
            "activatedAt": "2026-08-16T12:00:00.000Z",
            "documentUpdatedAt": "2026-08-16T00:00:00.000Z",
            "sourceUrl": "https://opencode.ai/docs/go/",
            "contentHash": "embedded-opencode-go-2026-08-16",
            "adjustmentPolicyVersion": "local-v4",
            "limits": { "window5h": 12.0, "windowWeek": 30.0, "windowMonth": 60.0 },
            "models": []
        }))
        .is_err(),
        "kernel snapshot id must not occupy revision"
    );
    assert_secret_free(&value);

    let mut peak = snapshot.models[0].clone();
    peak.cache_write = Some(0.375);
    peak.min_input_tokens = Some(256_001);
    peak.max_input_tokens = None;
    peak.time_window = PricingTimeWindow::Peak;
    let peak_value = serde_json::to_value(&peak).unwrap();
    assert_eq!(peak_value["cacheWrite"], 0.375);
    assert_eq!(peak_value["minInputTokens"], 256_001);
    assert_eq!(peak_value["maxInputTokens"], Value::Null);
    assert_eq!(peak_value["timeWindow"], "peak");
    assert_eq!(
        serde_json::to_value(PricingTimeWindow::OffPeak).unwrap(),
        json!("off_peak")
    );
}

#[test]
fn pricing_refresh_and_provider_pricing_emit_nullable_fields() {
    let refresh = PricingRefresh {
        snapshot: sample_pricing_snapshot(),
        refresh_status: PricingRefreshStatus::NeedsConfirmation,
        multiplier_changes: vec![PricingMultiplierChange {
            model_id: "grok-4.5".into(),
            current_multiplier: 4.0,
            official_multiplier: 5.0,
        }],
        official_content_hash: Some("official-hash".into()),
        error: None,
    };
    let refresh_value = serde_json::to_value(&refresh).unwrap();
    assert_eq!(refresh_value["refreshStatus"], "needs_confirmation");
    assert_eq!(refresh_value["officialContentHash"], "official-hash");
    assert_eq!(refresh_value["error"], Value::Null);
    assert_eq!(
        refresh_value["snapshot"]["pricingRevision"],
        "seed-2026-08-16-local-v4"
    );
    assert!(refresh_value.get("models").is_none());
    assert!(refresh_value.get("snapshotJson").is_none());
    assert_eq!(
        serde_json::to_value(PricingRefreshStatus::Success).unwrap(),
        json!("success")
    );
    assert_eq!(
        serde_json::to_value(PricingRefreshStatus::Unchanged).unwrap(),
        json!("unchanged")
    );
    assert_eq!(
        serde_json::to_value(PricingRefreshStatus::FailedNoChange).unwrap(),
        json!("failed_no_change")
    );
    assert_secret_free(&refresh_value);

    let provider = ProviderPricing {
        provider_id: "opencode".into(),

        availability: PricingAvailability::Available,
        snapshot: None,
        provider_snapshot: None,
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed-2026-08-16-local-v4".into(),
        provider_pricing_revision: "seed-2026-08-16-local-v4".into(),
    };
    let provider_value = serde_json::to_value(&provider).unwrap();
    assert_eq!(provider_value["snapshot"], Value::Null);
    assert_eq!(provider_value["availability"], "available");
    assert_eq!(provider_value["providerId"], "opencode");
    assert_eq!(provider_value["revision"], 11);
    assert!(provider_value.get("snapshotJson").is_none());
    assert_eq!(
        serde_json::to_value(PricingAvailability::Unavailable).unwrap(),
        json!("unavailable")
    );
    assert_eq!(
        serde_json::to_value(PricingAvailability::NotApplicable).unwrap(),
        json!("not_applicable")
    );
    assert_eq!(
        serde_json::to_value(PricingAvailability::Unpriced).unwrap(),
        json!("unpriced")
    );
    assert_secret_free(&provider_value);
}

#[test]
fn pricing_mutations_flatten_cas_require_pricing_revision_and_reject_unknown_fields() {
    let omitted: PricingRefreshUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "expectedPricingRevision": "seed-2026-08-16-local-v4"
    }))
    .unwrap();
    assert_eq!(omitted.expectation.expected_revision, 11);
    assert_eq!(omitted.expectation.process_generation, 9);
    assert_eq!(
        omitted.expected_pricing_revision,
        "seed-2026-08-16-local-v4"
    );
    assert!(omitted.policy.is_none());
    assert!(omitted.expected_official_content_hash.is_none());

    let confirmed: PricingRefreshUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "expectedPricingRevision": "seed-2026-08-16-local-v4",
        "policy": "keep_current",
        "expectedOfficialContentHash": "official-hash"
    }))
    .unwrap();
    assert_eq!(confirmed.policy, Some(PricingRefreshPolicy::KeepCurrent));
    assert_eq!(
        confirmed.expected_official_content_hash.as_deref(),
        Some("official-hash")
    );
    assert_eq!(
        serde_json::to_value(PricingRefreshPolicy::UseOfficial).unwrap(),
        json!("use_official")
    );

    assert!(
        serde_json::from_value::<PricingRefreshUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9
        }))
        .is_err(),
        "expectedPricingRevision is required"
    );
    assert!(
        serde_json::from_value::<PricingRefreshUpdate>(json!({
            "expected_revision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PricingRefreshUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expected_pricing_revision": "seed"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PricingRefreshUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed",
            "snapshotJson": "{}"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PricingRefreshUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed",
            "key": "sk-secret"
        }))
        .is_err()
    );

    let multipliers: PricingMultipliersUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "expectedPricingRevision": "seed-2026-08-16-local-v4",
        "multipliers": [{ "modelId": "grok-4.5", "multiplier": 4.0 }]
    }))
    .unwrap();
    assert_eq!(multipliers.multipliers[0].model_id, "grok-4.5");
    assert_eq!(multipliers.multipliers[0].multiplier, 4.0);
    assert!(
        serde_json::from_value::<PricingMultipliersUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "multipliers": []
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PricingMultipliersUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "expectedPricingRevision": "seed",
            "multipliers": [{ "model_id": "grok-4.5", "multiplier": 4.0 }]
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PricingMultiplierWrite>(json!({
            "modelId": "grok-4.5",
            "multiplier": 4.0,
            "token": "nope"
        }))
        .is_err()
    );
}

fn sample_gateway_status() -> GatewayStatus {
    GatewayStatus {
        running: true,
        port: 9042,
        upstream_base_url: "https://opencode.ai/zen/go/v1".into(),
        last_error: None,
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    }
}

fn sample_forward_log() -> ForwardLog {
    ForwardLog {
        id: 7,
        timestamp: "2026-08-16T12:00:00.000Z".into(),
        model: "glm-5".into(),
        account_id: "acct-1".into(),
        account_name: "go".into(),
        route_account_id: Some("acct-1".into()),
        provider_id: Some("opencode".into()),

        credential_account_id: Some("acct-1".into()),
        client_key_id: Some("key-1".into()),
        client_key_name: Some("Laptop".into()),
        status: "success".into(),
        http_status: Some(200),
        route: "proxy".into(),
        prompt_tokens: 1,
        completion_tokens: 2,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: Some(0.1),
        raw_cost_usd: Some(0.1),
        quota_debit: Some(0.1),
        effective_paid_cost_usd: Some(0.1),
        pricing_revision_id: Some("seed".into()),
        quota_multiplier: Some(1.0),
        local_adjustment_multiplier: Some(1.0),
        service_tier: None,
        cost_state: "priced".into(),
        error_message: None,
        request_id: Some("req-1".into()),
        attempt: Some(1),
        error_source: None,
        error_stage: None,
        duration_ms: Some(12),
        diagnostic: None,
        requested_model: Some("GLM-5".into()),
        resolved_alias: Some("glm-5".into()),
        upstream_model: Some("glm-5".into()),
        native_cost_value: Some(0.1),
        native_cost_unit: Some("usd".into()),
        native_cost_currency: Some("USD".into()),
    }
}

#[test]
fn observability_dtos_emit_camel_case_nulls_and_stay_secret_free() {
    let status = sample_gateway_status();
    let status_value = serde_json::to_value(&status).unwrap();
    assert_eq!(status_value["running"], true);
    assert_eq!(status_value["port"], 9042);
    assert_eq!(
        status_value["upstreamBaseUrl"],
        "https://opencode.ai/zen/go/v1"
    );
    assert_eq!(status_value["lastError"], Value::Null);
    assert_eq!(status_value["revision"], 11);
    assert_eq!(status_value["processGeneration"], 9);
    assert_eq!(status_value["pricingRevision"], "seed");
    assert!(status_value.get("key").is_none());
    assert!(status_value.get("gatewayKey").is_none());
    assert!(status_value.get("primaryKey").is_none());
    assert!(status_value.get("upstream_base_url").is_none());
    assert_secret_free(&status_value);

    let models = ApplicationModels {
        models: vec!["grok-4.5".into(), "minimax-m2.7-highspeed".into()],
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let models_value = serde_json::to_value(&models).unwrap();
    assert_eq!(
        models_value["models"],
        json!(["grok-4.5", "minimax-m2.7-highspeed"])
    );
    assert_eq!(models_value["revision"], 11);
    assert!(models_value.as_object().unwrap().contains_key("models"));

    let summary = DashboardSummary {
        total_accounts: 2,
        available_accounts: 1,
        gateway_running: true,
        today_cost: 1.5,
        week_cost: 2.5,
        month_cost: 3.5,
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let summary_value = serde_json::to_value(&summary).unwrap();
    assert_eq!(summary_value["totalAccounts"], 2);
    assert_eq!(summary_value["availableAccounts"], 1);
    assert_eq!(summary_value["gatewayRunning"], true);
    assert!(summary_value.get("total_accounts").is_none());

    let daily = DailyTokensByModel {
        items: vec![DailyModelTokens {
            date: "2026-08-16".into(),
            model: "glm-5".into(),
            tokens: 1250,
        }],
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let daily_value = serde_json::to_value(&daily).unwrap();
    assert_eq!(daily_value["items"][0]["date"], "2026-08-16");
    assert_eq!(daily_value["items"][0]["model"], "glm-5");
    assert_eq!(daily_value["items"][0]["tokens"], 1250);

    let log = sample_forward_log();
    let log_value = serde_json::to_value(&log).unwrap();
    assert_eq!(log_value["requestedModel"], "GLM-5");
    assert_eq!(log_value["resolvedAlias"], "glm-5");
    assert_eq!(log_value["upstreamModel"], "glm-5");
    assert_eq!(log_value["accountId"], "acct-1");
    assert_eq!(log_value["httpStatus"], 200);
    assert_eq!(log_value["costState"], "priced");
    assert_eq!(log_value["errorMessage"], Value::Null);
    assert_eq!(log_value["serviceTier"], Value::Null);
    assert_eq!(log_value["diagnostic"], Value::Null);
    assert!(log_value.get("requestedAlias").is_none());
    assert!(log_value.get("requested_alias").is_none());
    assert!(log_value.get("requested_model").is_none());
    assert_secret_free(&log_value);

    let page = ForwardLogs {
        items: vec![log],
        summary: ForwardLogSummary {
            total_requests: 1,
            prompt_tokens: 1,
            completion_tokens: 2,
            cached_tokens: 0,
            cost: 0.1,
        },
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let page_value = serde_json::to_value(&page).unwrap();
    assert_eq!(page_value["summary"]["totalRequests"], 1);
    assert_eq!(page_value["summary"]["promptTokens"], 1);
    assert!(
        page_value
            .get("summary")
            .unwrap()
            .get("total_requests")
            .is_none()
    );

    let gateway = GatewayLogs {
        items: vec![GatewayLog {
            id: 3,
            level: "info".into(),
            category: "gateway".into(),
            message: "started".into(),
            created_at: "2026-08-16T12:00:00.000Z".into(),
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        }],
        revision: 11,
        process_generation: 9,
        pricing_revision: "seed".into(),
    };
    let gateway_value = serde_json::to_value(&gateway).unwrap();
    assert_eq!(
        gateway_value["items"][0]["createdAt"],
        "2026-08-16T12:00:00.000Z"
    );
    assert_eq!(gateway_value["items"][0]["requestId"], Value::Null);
    assert_eq!(gateway_value["items"][0]["diagnostic"], Value::Null);
    assert_secret_free(&gateway_value);
}

#[test]
fn observability_query_objects_deny_unknown_and_snake_case_fields() {
    let empty: ForwardLogQuery = serde_json::from_value(json!({})).unwrap();
    assert!(empty.account_id.is_none());
    assert!(empty.limit.is_none());

    let parsed: ForwardLogQuery = serde_json::from_value(json!({
        "limit": 20,
        "offset": 10,
        "accountId": "acct-1",
        "sortBy": "prompt_tokens",
        "sortOrder": "asc"
    }))
    .unwrap();
    assert_eq!(parsed.limit, Some(20));
    assert_eq!(parsed.account_id.as_deref(), Some("acct-1"));
    assert_eq!(parsed.sort_by.as_deref(), Some("prompt_tokens"));

    assert!(serde_json::from_value::<ForwardLogQuery>(json!({ "account_id": "acct-1" })).is_err());
    assert!(
        serde_json::from_value::<ForwardLogQuery>(json!({
            "accountId": "acct-1",
            "unknown": true
        }))
        .is_err()
    );
    assert!(serde_json::from_value::<GatewayLogQuery>(json!({ "request_id": "r" })).is_err());
    assert!(serde_json::from_value::<DailyTokensQuery>(json!({ "Days": 7 })).is_err());
    let days: DailyTokensQuery = serde_json::from_value(json!({ "days": 7 })).unwrap();
    assert_eq!(days.days, Some(7));
}

#[test]
fn observability_catalog_registers_new_defs_without_reshaping_the_prefix() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in OBSERVABILITY_CATALOG_TYPES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }
    let status_required = defs["GatewayStatus"]["required"].as_array().unwrap();
    for field in [
        "running",
        "port",
        "upstreamBaseUrl",
        "lastError",
        "revision",
        "processGeneration",
        "pricingRevision",
    ] {
        assert!(
            status_required.iter().any(|value| value == field),
            "{field} must stay required"
        );
    }
    assert!(
        !defs["GatewayStatus"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("key")
    );
    let forward_required = defs["ForwardLog"]["required"].as_array().unwrap();
    for field in [
        "requestedModel",
        "resolvedAlias",
        "upstreamModel",
        "diagnostic",
    ] {
        assert!(
            forward_required.iter().any(|value| value == field),
            "{field} must stay required T|null"
        );
    }
    assert!(
        !defs["ForwardLog"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("requestedAlias")
    );
    assert_eq!(defs["ForwardLogQuery"]["additionalProperties"], false);
    assert_eq!(defs["GatewayLogQuery"]["additionalProperties"], false);
    assert_eq!(defs["DailyTokensQuery"]["additionalProperties"], false);
}

#[test]
fn usage_responses_emit_camel_case_nulls_and_reject_unknown_request_fields() {
    let usage = UsageWindow {
        account_id: "acct-1".into(),
        window_5h: 6.0,
        window_week: 6.0,
        window_month: 6.0,
        resets_in_5h: None,
        resets_in_week: None,
        resets_in_month: None,
        revision: 11,
        process_generation: 9,
        pricing_revision: Some("seed".into()),
    };
    let usage_value = serde_json::to_value(&usage).unwrap();
    assert_eq!(usage_value["accountId"], "acct-1");
    assert_eq!(usage_value["window5h"], 6.0);
    assert_eq!(usage_value["windowWeek"], 6.0);
    assert_eq!(usage_value["windowMonth"], 6.0);
    assert_eq!(usage_value["resetsIn5h"], Value::Null);
    assert_eq!(usage_value["resetsInWeek"], Value::Null);
    assert_eq!(usage_value["resetsInMonth"], Value::Null);
    assert_eq!(usage_value["revision"], 11);
    assert_eq!(usage_value["processGeneration"], 9);
    assert_eq!(usage_value["pricingRevision"], "seed");
    assert!(usage_value.get("window_5h").is_none());
    assert!(usage_value.get("resets_in_5h").is_none());
    assert_secret_free(&usage_value);

    let goat = UsageWindow {
        pricing_revision: None,
        ..usage.clone()
    };
    assert_eq!(
        serde_json::to_value(&goat).unwrap()["pricingRevision"],
        Value::Null
    );

    let mutation = UsageMutation {
        usage: goat,
        revision: 11,
        process_generation: 9,
    };
    let mutation_value = serde_json::to_value(&mutation).unwrap();
    assert_eq!(mutation_value["revision"], 11);
    assert_eq!(mutation_value["processGeneration"], 9);
    assert_eq!(mutation_value["usage"]["pricingRevision"], Value::Null);
    assert_secret_free(&mutation_value);

    let omitted: AccountUsageUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "window": "window_5h",
        "percent": 50.0
    }))
    .unwrap();
    assert_eq!(omitted.expectation.expected_revision, 11);
    assert_eq!(omitted.window, "window_5h");
    assert_eq!(omitted.percent, 50.0);
    assert!(omitted.resets_in_minutes.is_none());
    let with_reset: AccountUsageUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "window": "window_5h",
        "percent": 50.0,
        "resetsInMinutes": 180
    }))
    .unwrap();
    assert_eq!(with_reset.resets_in_minutes, Some(180));
    assert!(
        serde_json::from_value::<AccountUsageUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "window": "window_5h",
            "percent": 50.0,
            "resets_in_minutes": 180
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AccountUsageUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "window": "window_5h",
            "percent": 50.0,
            "key": "sk-secret"
        }))
        .is_err()
    );

    let provider = ProviderUsage {
        account_id: "acct-1".into(),
        provider_id: "opencode".into(),

        availability: UsageAvailability::Available,
        experimental: false,
        free_cooldown_until: None,
        quota_windows: vec![QuotaWindow {
            account_id: "acct-1".into(),
            window_kind: "five_hours".into(),
            used: 6.0,
            limit_value: Some(12.0),
            started_at: None,
            resets_at: None,
            calibration_offset: 0.0,
            unit: "usd".into(),
            source: "opencode-go-live".into(),
            observed_at: None,
            updated_at: "2026-08-16T12:00:00Z".into(),
        }],
        credit_balances: Vec::new(),
        sync_state: None,
        revision: 11,
        process_generation: 9,
        pricing_revision: Some("seed".into()),
    };
    let provider_value = serde_json::to_value(&provider).unwrap();
    assert_eq!(provider_value["availability"], "available");
    assert_eq!(provider_value["freeCooldownUntil"], Value::Null);
    assert_eq!(provider_value["syncState"], Value::Null);
    assert_eq!(
        provider_value["quotaWindows"][0]["windowKind"],
        "five_hours"
    );
    assert_eq!(provider_value["quotaWindows"][0]["limitValue"], 12.0);
    assert_eq!(provider_value["quotaWindows"][0]["startedAt"], Value::Null);
    assert_eq!(provider_value["pricingRevision"], "seed");
    assert!(provider_value.get("quota_windows").is_none());
    assert_eq!(
        serde_json::to_value(UsageAvailability::LocalState).unwrap(),
        json!("local_state")
    );
    assert_eq!(
        serde_json::to_value(UsageAvailability::Unavailable).unwrap(),
        json!("unavailable")
    );
    assert_secret_free(&provider_value);

    let schema = contract_schema();
    let usage_required = schema["$defs"]["UsageWindow"]["required"]
        .as_array()
        .unwrap();
    for field in [
        "resetsIn5h",
        "resetsInWeek",
        "resetsInMonth",
        "pricingRevision",
    ] {
        assert!(
            usage_required.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
    let provider_required = schema["$defs"]["ProviderUsage"]["required"]
        .as_array()
        .unwrap();
    for field in ["freeCooldownUntil", "syncState", "pricingRevision"] {
        assert!(
            provider_required.iter().any(|value| value == field),
            "{field} must stay required so responses emit T|null"
        );
    }
    let request = &schema["$defs"]["AccountUsageUpdate"];
    assert_eq!(request["additionalProperties"], false);
    let request_required = request["required"].as_array().unwrap();
    assert!(
        request_required
            .iter()
            .any(|value| value == "expectedRevision")
    );
    assert!(
        request_required
            .iter()
            .any(|value| value == "processGeneration")
    );
    assert!(request_required.iter().any(|value| value == "window"));
    assert!(request_required.iter().any(|value| value == "percent"));
    assert!(
        !request_required
            .iter()
            .any(|value| value == "resetsInMinutes")
    );
}

#[test]
fn auth_status_is_camel_case_and_secret_free() {
    let status = AuthStatus {
        local: false,
        initialized: true,
        authenticated: true,
        revision: 11,
        process_generation: 9,
    };
    let value = serde_json::to_value(&status).unwrap();
    assert_eq!(
        value,
        json!({
            "local": false,
            "initialized": true,
            "authenticated": true,
            "revision": 11,
            "processGeneration": 9 })
    );
    let object = value.as_object().unwrap();
    for forbidden in [
        "password",
        "key",
        "token",
        "cipher",
        "secret",
        "cookie",
        "sessionToken",
        "gatewayKey",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "AuthStatus must not expose {forbidden}"
        );
    }
}

#[test]
fn auth_request_dtos_require_cas_and_reject_unknown_fields() {
    let register: AuthRegister = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "username": "admin",
        "password": "password123"
    }))
    .unwrap();
    assert_eq!(register.expectation.expected_revision, 3);
    assert_eq!(register.expectation.process_generation, 9);
    assert_eq!(register.username, "admin");
    assert_eq!(register.password, "password123");

    let login: AuthLogin = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9,
        "username": "admin",
        "password": "password123"
    }))
    .unwrap();
    assert_eq!(login.username, "admin");

    let logout: AuthLogout = serde_json::from_value(json!({
        "expectedRevision": 3,
        "processGeneration": 9
    }))
    .unwrap();
    assert_eq!(logout.expectation.expected_revision, 3);

    assert!(
        serde_json::from_value::<AuthRegister>(json!({
            "username": "admin",
            "password": "password123"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AuthLogin>(json!({
            "expected_revision": 3,
            "processGeneration": 9,
            "username": "admin",
            "password": "password123"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<AuthLogout>(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "username": "admin"
        }))
        .is_err()
    );
    for unknown in ["token", "secret", "key", "cipher", "sessionToken"] {
        let mut body = json!({
            "expectedRevision": 3,
            "processGeneration": 9,
            "username": "admin",
            "password": "password123"
        });
        body[unknown] = json!("must-not-be-accepted");
        assert!(
            serde_json::from_value::<AuthRegister>(body.clone()).is_err(),
            "{unknown}"
        );
        assert!(
            serde_json::from_value::<AuthLogin>(body).is_err(),
            "{unknown}"
        );
    }
}

#[test]
fn proxy_test_catalog_registers_without_cas_or_secret_fields() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in PROXY_TEST_CATALOG_TYPES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }

    let request_required = defs["ProxyTestRequest"]["required"]
        .as_array()
        .expect("ProxyTestRequest.required");
    assert_eq!(request_required, &vec![json!("proxyMode")]);
    assert_eq!(defs["ProxyTestRequest"]["additionalProperties"], false);
    let request_props = defs["ProxyTestRequest"]["properties"]
        .as_object()
        .expect("ProxyTestRequest.properties");
    assert!(request_props.contains_key("proxyUrl"));
    assert!(request_props.contains_key("proxyListDirection"));
    for forbidden in [
        "expectedRevision",
        "processGeneration",
        "upstreamBaseUrl",
        "upstream_base_url",
        "proxy_url",
        "key",
        "gatewayKey",
        "primaryKey",
    ] {
        assert!(
            !request_props.contains_key(forbidden),
            "ProxyTestRequest must not expose {forbidden}"
        );
    }

    let response_required = defs["ProxyTestResponse"]["required"]
        .as_array()
        .expect("ProxyTestResponse.required");
    assert_eq!(
        response_required,
        &vec![
            json!("proxyMode"),
            json!("status"),
            json!("latencyMs"),
            json!("revision"),
            json!("processGeneration"),
        ]
    );
    let response_props = defs["ProxyTestResponse"]["properties"]
        .as_object()
        .expect("ProxyTestResponse.properties");
    for forbidden in [
        "proxyUrl",
        "upstreamBaseUrl",
        "body",
        "key",
        "gatewayKey",
        "latency_ms",
    ] {
        assert!(
            !response_props.contains_key(forbidden),
            "ProxyTestResponse must not expose {forbidden}"
        );
    }
}

#[test]
fn claude_desktop_models_are_camel_case_cas_and_secret_free() {
    let models = ClaudeDesktopModels {
        sonnet: "glm-5.2".into(),
        opus: "grok-4.5".into(),
        haiku: "mimo-v2.5".into(),
        revision: 11,
        process_generation: 9,
    };
    let value = serde_json::to_value(&models).unwrap();
    assert_eq!(
        value,
        json!({
            "sonnet": "glm-5.2",
            "opus": "grok-4.5",
            "haiku": "mimo-v2.5",
            "revision": 11,
            "processGeneration": 9 })
    );
    let object = value.as_object().unwrap();
    for forbidden in [
        "pricingRevision",
        "pricing_revision",
        "key",
        "gatewayKey",
        "gateway_key",
        "primaryKey",
        "cipher",
        "secret",
        "token",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "ClaudeDesktopModels must not expose {forbidden}"
        );
    }

    let parsed: ClaudeDesktopModelsUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9,
        "sonnet": "glm-5.2",
        "opus": "",
        "haiku": "mimo-v2.5"
    }))
    .unwrap();
    assert_eq!(parsed.expectation.expected_revision, 11);
    assert_eq!(parsed.expectation.process_generation, 9);
    assert_eq!(parsed.sonnet, "glm-5.2");
    assert_eq!(parsed.opus, "");
    assert_eq!(parsed.haiku, "mimo-v2.5");

    assert!(
        serde_json::from_value::<ClaudeDesktopModelsUpdate>(json!({
            "processGeneration": 9,
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": ""
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ClaudeDesktopModelsUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "opus": "",
            "haiku": ""
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ClaudeDesktopModelsUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "sonnet": "glm-5.2",
            "opus": "",
            "haiku": "",
            "gatewayKey": "ocg-secret"
        }))
        .is_err()
    );
}

#[test]
fn claude_desktop_catalog_registers_required_roles_and_cas() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in CLAUDE_DESKTOP_CATALOG_TYPES {
        assert!(defs.contains_key(*name), "schema missing {name}");
        assert_eq!(defs[*name]["additionalProperties"], false);
    }

    let response_required = defs["ClaudeDesktopModels"]["required"]
        .as_array()
        .expect("ClaudeDesktopModels.required");
    assert_eq!(
        response_required,
        &vec![
            json!("sonnet"),
            json!("opus"),
            json!("haiku"),
            json!("revision"),
            json!("processGeneration"),
        ]
    );
    let response_props = defs["ClaudeDesktopModels"]["properties"]
        .as_object()
        .expect("ClaudeDesktopModels.properties");
    assert!(!response_props.contains_key("pricingRevision"));
    assert!(!response_props.contains_key("key"));
    assert!(!response_props.contains_key("gatewayKey"));

    let update_required = defs["ClaudeDesktopModelsUpdate"]["required"]
        .as_array()
        .expect("ClaudeDesktopModelsUpdate.required");
    assert_eq!(
        update_required,
        &vec![
            json!("expectedRevision"),
            json!("processGeneration"),
            json!("sonnet"),
            json!("opus"),
            json!("haiku"),
        ]
    );
    let update_props = defs["ClaudeDesktopModelsUpdate"]["properties"]
        .as_object()
        .expect("ClaudeDesktopModelsUpdate.properties");
    for forbidden in ["key", "gatewayKey", "primaryKey", "pricingRevision"] {
        assert!(
            !update_props.contains_key(forbidden),
            "ClaudeDesktopModelsUpdate must not expose {forbidden}"
        );
    }
}

#[test]
fn managed_key_verify_request_requires_cas_and_write_only_key() {
    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    assert!(defs.contains_key("AccountManagedKeyVerify"));
    assert_eq!(
        defs["AccountManagedKeyVerify"]["additionalProperties"],
        false
    );
    let required = defs["AccountManagedKeyVerify"]["required"]
        .as_array()
        .expect("AccountManagedKeyVerify.required");
    for field in ["expectedRevision", "processGeneration", "key"] {
        assert!(
            required.iter().any(|value| value == field),
            "{field} must be required"
        );
    }
    assert_eq!(required.len(), 3);
    let props = defs["AccountManagedKeyVerify"]["properties"]
        .as_object()
        .expect("AccountManagedKeyVerify.properties");
    assert_eq!(props["key"]["type"], "string");
    for forbidden in [
        "keyCipher",
        "gatewayKey",
        "primaryKey",
        "setupStep",
        "account",
        "expected_revision",
    ] {
        assert!(
            !props.contains_key(forbidden),
            "AccountManagedKeyVerify must not expose {forbidden}"
        );
    }
}

#[test]
fn usage_refresh_dtos_are_camel_case_secret_free_and_append_only() {
    let omitted: UsageRefreshUpdate = serde_json::from_value(json!({
        "expectedRevision": 11,
        "processGeneration": 9
    }))
    .unwrap();
    assert_eq!(omitted.expectation.expected_revision, 11);
    assert_eq!(omitted.expectation.process_generation, 9);
    assert!(
        serde_json::from_value::<UsageRefreshUpdate>(json!({
            "expected_revision": 11,
            "processGeneration": 9
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<UsageRefreshUpdate>(json!({
            "expectedRevision": 11,
            "processGeneration": 9,
            "key": "sk-secret"
        }))
        .is_err()
    );

    let refresh = UsageRefresh {
        usage: UsageWindow {
            account_id: "acct-1".into(),
            window_5h: 6.0,
            window_week: 6.0,
            window_month: 6.0,
            resets_in_5h: None,
            resets_in_week: None,
            resets_in_month: None,
            revision: 11,
            process_generation: 9,
            pricing_revision: Some("seed".into()),
        },
        source: "official_go_usage".into(),
        last_success_at: "2026-08-18T12:00:00+00:00".into(),
        next_allowed_at: "2026-08-18T12:00:15+00:00".into(),
        revision: 11,
        process_generation: 9,
    };
    let value = serde_json::to_value(&refresh).unwrap();
    assert_eq!(value["source"], "official_go_usage");
    assert_eq!(value["lastSuccessAt"], "2026-08-18T12:00:00+00:00");
    assert_eq!(value["nextAllowedAt"], "2026-08-18T12:00:15+00:00");
    assert_eq!(value["usage"]["window5h"], 6.0);
    assert_eq!(value["usage"]["pricingRevision"], "seed");
    assert_eq!(value["revision"], 11);
    assert_eq!(value["processGeneration"], 9);
    assert!(value.get("last_success_at").is_none());
    assert!(value.get("next_allowed_at").is_none());
    assert!(value.get("fetched_at").is_none());
    assert_secret_free(&value);

    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in USAGE_REFRESH_CATALOG_TYPES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }
    let request = &schema["$defs"]["UsageRefreshUpdate"];
    assert_eq!(request["additionalProperties"], false);
    let request_required = request["required"].as_array().unwrap();
    assert!(
        request_required
            .iter()
            .any(|value| value == "expectedRevision")
    );
    assert!(
        request_required
            .iter()
            .any(|value| value == "processGeneration")
    );
    let response_required = schema["$defs"]["UsageRefresh"]["required"]
        .as_array()
        .unwrap();
    for field in [
        "usage",
        "source",
        "lastSuccessAt",
        "nextAllowedAt",
        "revision",
        "processGeneration",
    ] {
        assert!(
            response_required.iter().any(|value| value == field),
            "{field} must stay required"
        );
    }
    let response_props = schema["$defs"]["UsageRefresh"]["properties"]
        .as_object()
        .unwrap();
    assert!(!response_props.contains_key("last_success_at"));
    assert!(!response_props.contains_key("next_allowed_at"));
    assert!(!response_props.contains_key("fetched_at"));
    assert!(!response_props.contains_key("key"));

    let throttle = UsageRefreshThrottleError {
        code: ERROR_THROTTLED.into(),
        message: "retry later".into(),
        current_revision: Some(11),
        process_generation: Some(9),
        next_allowed_at: "2026-08-18T12:00:15+00:00".into(),
    };
    let throttle_value = serde_json::to_value(&throttle).unwrap();
    assert_eq!(throttle_value["code"], ERROR_THROTTLED);
    assert_eq!(throttle_value["nextAllowedAt"], "2026-08-18T12:00:15+00:00");
    assert_eq!(
        schema["$defs"]["UsageRefreshThrottleError"]["additionalProperties"],
        false
    );
    assert_secret_free(&throttle_value);
}
