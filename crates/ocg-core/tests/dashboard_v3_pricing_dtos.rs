//! Dashboard V3 pricing DTO catalog: prefix stability, required/optional sets,
//! T|null responses, request omission, and secrecy.

use ocg_core::dashboard_v3::{CATALOG_TYPE_NAMES, contract_schema};
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;

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
    "AccountAcknowledgement",
    "AccountCreate",
    "AccountManagedCreate",
    "AccountUpdate",
    "AccountOrder",
    "AccountSetupUpdate",
    "AccountCustomConfigUpdate",
    "AccountCustomConfigWrite",
    "AccountModelCapabilitiesUpdate",
    "AccountModelCapabilityWrite",
    "AccountAcknowledgementCreate",
    "AccountAcknowledgementWrite",
];

const PROVIDER_CATALOG_TYPES: &[&str] = &[
    "ProviderCatalog",
    "ProviderCatalogEntry",
    "ProviderCatalogFormField",
    "ProviderCatalogRiskNotice",
    "ProviderModelCapability",
    "ZenFreeSettings",
    "ZenFreeSettingsUpdate",
    "ZenFreeModels",
    "ZenFreeModel",
    "ProviderContracts",
    "ProviderContractGroup",
    "CustomEndpointContract",
    "ProviderOfferingChoice",
    "ProviderAccountChoice",
    "ProtocolSwitches",
    "EffectiveCatalog",
    "EffectiveModelContract",
    "EffectiveModelProtocols",
    "EffectiveProtocolEvidence",
    "CapabilitySummary",
    "CardCapabilitySummary",
    "ProtocolSwitchUpdate",
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

const CUSTOM_DISCOVERY_CATALOG_TYPES: &[&str] = &[
    "CustomModelDiscoveryRequest",
    "CustomModelDiscoveryResponse",
];

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

fn defs(schema: &Value) -> &Map<String, Value> {
    schema["$defs"].as_object().expect("catalog $defs")
}

fn properties<'a>(defs: &'a Map<String, Value>, name: &str) -> &'a Map<String, Value> {
    defs[name]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("{name}.properties"))
}

fn required_fields<'a>(defs: &'a Map<String, Value>, name: &str) -> Vec<&'a str> {
    defs[name]["required"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}.required"))
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

fn enum_values<'a>(defs: &'a Map<String, Value>, name: &str) -> Vec<&'a str> {
    defs[name]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("{name}.enum"))
        .iter()
        .filter_map(Value::as_str)
        .collect()
}

fn schema_field_names<'a>(value: &'a Value, acc: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            if let Some(properties) = map.get("properties").and_then(Value::as_object) {
                acc.extend(properties.keys().map(String::as_str));
                for nested in properties.values() {
                    schema_field_names(nested, acc);
                }
            }
            for (key, nested) in map {
                if key != "properties" {
                    schema_field_names(nested, acc);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                schema_field_names(item, acc);
            }
        }
        _ => {}
    }
}

fn allows_null(schema: &Value) -> bool {
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if let Some(types) = schema.get("type").and_then(Value::as_array) {
        if types.iter().any(|value| value == "null") {
            return true;
        }
    }
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        return any_of.iter().any(allows_null);
    }
    false
}

fn is_numeric(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind == "number" || kind == "integer",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| {
            kind.as_str()
                .is_some_and(|kind| kind == "number" || kind == "integer")
        }),
        _ => schema
            .get("anyOf")
            .and_then(Value::as_array)
            .is_some_and(|items| items.iter().any(is_numeric)),
    }
}

fn frozen_schema() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/dashboard-api-v3.schema.json");
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&text).expect("checked-in V3 schema JSON")
}

#[test]
fn catalog_type_names_keep_the_frozen_prefix_and_append_pricing() {
    let prefix_len = ACCOUNTS_CATALOG_PREFIX.len() + PROVIDER_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[..ACCOUNTS_CATALOG_PREFIX.len()],
        ACCOUNTS_CATALOG_PREFIX
    );
    assert_eq!(
        &CATALOG_TYPE_NAMES[ACCOUNTS_CATALOG_PREFIX.len()..prefix_len],
        PROVIDER_CATALOG_TYPES
    );
    let pricing_end = prefix_len + PRICING_CATALOG_TYPES.len();
    assert_eq!(
        &CATALOG_TYPE_NAMES[prefix_len..pricing_end],
        PRICING_CATALOG_TYPES
    );
    assert_eq!(
        &CATALOG_TYPE_NAMES[pricing_end..],
        CUSTOM_DISCOVERY_CATALOG_TYPES
    );
    assert_eq!(
        CATALOG_TYPE_NAMES.len(),
        pricing_end + CUSTOM_DISCOVERY_CATALOG_TYPES.len()
    );
}

#[test]
fn existing_defs_stay_byte_identical_to_the_frozen_provider_contract() {
    let live = contract_schema();
    let frozen = frozen_schema();
    let live_defs = defs(&live);
    let frozen_defs = defs(&frozen);
    for (name, frozen_def) in frozen_defs {
        assert_eq!(
            live_defs
                .get(name)
                .unwrap_or_else(|| panic!("live schema missing frozen $defs/{name}")),
            frozen_def,
            "existing $defs/{name} drifted"
        );
    }

    let live_any_of = live["anyOf"].as_array().expect("live anyOf");
    let frozen_any_of = frozen["anyOf"].as_array().expect("frozen anyOf");
    assert_eq!(
        &live_any_of[..frozen_any_of.len()],
        frozen_any_of.as_slice(),
        "catalog anyOf prefix drifted"
    );
    for (index, name) in CATALOG_TYPE_NAMES.iter().enumerate() {
        assert_eq!(
            live_any_of[index]["$ref"],
            format!("#/$defs/{name}"),
            "anyOf drifted at {index}"
        );
    }
}

#[test]
fn pricing_schema_registers_required_nulls_and_omittable_requests() {
    let schema = contract_schema();
    let defs = defs(&schema);
    for name in CATALOG_TYPE_NAMES {
        assert!(defs.contains_key(*name), "schema missing {name}");
    }

    assert_eq!(
        required_fields(defs, "PricingSnapshot"),
        vec![
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
        ]
    );
    let snapshot = properties(defs, "PricingSnapshot");
    assert_eq!(snapshot["revision"]["type"], "integer");
    assert_eq!(snapshot["pricingRevision"]["type"], "string");
    assert!(!snapshot.contains_key("snapshotJson"));
    assert!(!snapshot.contains_key("snapshot_json"));

    assert_eq!(
        required_fields(defs, "PricingLimits"),
        vec!["window5h", "windowWeek", "windowMonth"]
    );
    let limits = properties(defs, "PricingLimits");
    for field in ["window5h", "windowWeek", "windowMonth"] {
        assert!(is_numeric(&limits[field]), "{field} must be numeric");
        assert!(!allows_null(&limits[field]), "{field} is required number");
    }
    assert!(!limits.contains_key("window_5h"));
    assert!(!limits.contains_key("window_week"));
    assert!(!limits.contains_key("window_month"));

    assert_eq!(
        required_fields(defs, "PricingModel"),
        vec![
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
        ]
    );
    let model = properties(defs, "PricingModel");
    for field in ["input", "output", "cacheRead", "usage", "quotaMultiplier"] {
        assert!(is_numeric(&model[field]), "{field} must be numeric");
        assert!(!allows_null(&model[field]), "{field} is required number");
    }
    for field in ["cacheWrite", "minInputTokens", "maxInputTokens"] {
        assert!(is_numeric(&model[field]), "{field} must be numeric|null");
        assert!(
            allows_null(&model[field]),
            "{field} must stay required T|null"
        );
    }
    assert!(!model.contains_key("model_id"));
    assert!(!model.contains_key("cache_write"));

    assert_eq!(
        required_fields(defs, "PricingAdjustment"),
        vec!["label", "multiplier", "appliesTo"]
    );
    assert!(is_numeric(
        &properties(defs, "PricingAdjustment")["multiplier"]
    ));
    assert!(!properties(defs, "PricingAdjustment").contains_key("applies_to"));

    assert_eq!(
        enum_values(defs, "PricingTimeWindow"),
        vec!["always", "off_peak", "peak"]
    );

    assert_eq!(
        required_fields(defs, "PricingRefresh"),
        vec![
            "snapshot",
            "refreshStatus",
            "multiplierChanges",
            "officialContentHash",
            "error",
        ]
    );
    let refresh = properties(defs, "PricingRefresh");
    assert!(allows_null(&refresh["officialContentHash"]));
    assert!(allows_null(&refresh["error"]));
    assert!(!allows_null(&refresh["snapshot"]));
    assert!(!refresh.contains_key("refresh_status"));
    assert!(!refresh.contains_key("models"));

    assert_eq!(
        enum_values(defs, "PricingRefreshStatus"),
        vec![
            "success",
            "unchanged",
            "needs_confirmation",
            "failed_no_change",
        ]
    );

    assert_eq!(
        required_fields(defs, "PricingMultiplierChange"),
        vec!["modelId", "currentMultiplier", "officialMultiplier"]
    );
    let change = properties(defs, "PricingMultiplierChange");
    assert!(is_numeric(&change["currentMultiplier"]));
    assert!(is_numeric(&change["officialMultiplier"]));
    assert!(!change.contains_key("model_id"));

    let refresh_update_required = required_fields(defs, "PricingRefreshUpdate");
    assert_eq!(
        refresh_update_required,
        vec![
            "expectedRevision",
            "processGeneration",
            "expectedPricingRevision",
        ]
    );
    assert!(!refresh_update_required.contains(&"policy"));
    assert!(!refresh_update_required.contains(&"expectedOfficialContentHash"));
    let refresh_update = properties(defs, "PricingRefreshUpdate");
    assert!(refresh_update.contains_key("policy"));
    assert!(refresh_update.contains_key("expectedOfficialContentHash"));
    assert!(!refresh_update.contains_key("expected_revision"));
    assert!(!refresh_update.contains_key("expected_pricing_revision"));
    assert_eq!(defs["PricingRefreshUpdate"]["additionalProperties"], false);

    assert_eq!(
        enum_values(defs, "PricingRefreshPolicy"),
        vec!["keep_current", "use_official"]
    );

    assert_eq!(
        required_fields(defs, "PricingMultipliersUpdate"),
        vec![
            "expectedRevision",
            "processGeneration",
            "expectedPricingRevision",
            "multipliers",
        ]
    );
    assert_eq!(
        defs["PricingMultipliersUpdate"]["additionalProperties"],
        false
    );
    assert!(!properties(defs, "PricingMultipliersUpdate").contains_key("expected_revision"));

    assert_eq!(
        required_fields(defs, "PricingMultiplierWrite"),
        vec!["modelId", "multiplier"]
    );
    assert!(is_numeric(
        &properties(defs, "PricingMultiplierWrite")["multiplier"]
    ));
    assert_eq!(
        defs["PricingMultiplierWrite"]["additionalProperties"],
        false
    );

    assert_eq!(
        required_fields(defs, "ProviderPricing"),
        vec![
            "providerId",
            "offeringId",
            "availability",
            "snapshot",
            "revision",
            "processGeneration",
            "pricingRevision",
        ]
    );
    let provider = properties(defs, "ProviderPricing");
    assert!(allows_null(&provider["snapshot"]));
    assert!(!provider.contains_key("snapshotJson"));
    assert_eq!(provider["revision"]["type"], "integer");
    assert_eq!(provider["pricingRevision"]["type"], "string");

    assert_eq!(
        enum_values(defs, "PricingAvailability"),
        vec!["available", "unavailable", "not_applicable", "unpriced",]
    );
}

#[test]
fn pricing_dto_schema_has_no_secret_or_snapshot_json_fields() {
    let schema = contract_schema();
    let defs = defs(&schema);
    for name in PRICING_CATALOG_TYPES {
        let mut fields = Vec::new();
        schema_field_names(&defs[*name], &mut fields);
        for field in fields {
            assert!(
                !SECRET_FIELD_NAMES.contains(&field),
                "{name} schema leaked secret-bearing field {field}"
            );
        }
        let encoded = defs[*name].to_string();
        for secret in ["sk-secret", "ocg-secret", "pw-secret", "user:pass@"] {
            assert!(
                !encoded.contains(secret),
                "{name} schema leaked secret sample {secret}"
            );
        }
    }
    assert!(
        properties(defs, "ConnectionInfo").contains_key("primaryKey"),
        "ConnectionInfo remains the only secret-bearing V3 DTO"
    );
}
