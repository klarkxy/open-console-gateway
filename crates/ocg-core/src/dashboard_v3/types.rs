//! Shared Dashboard V3 wire types and the JSON Schema catalog.
//!
//! Response objects always serialize nullable fields as `T | null` (never omitted).
//! Request optional fields may be omitted; `expectedRevision` is required on every
//! control-plane mutation. Plaintext keys must not appear on `Settings` —
//! `ConnectionInfo` is the only secret-bearing V3 DTO.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::state::CoreState;

/// JSON Schema `$defs` names for the kernel catalog.
///
/// Later leases append new names here and register the matching DTO. Existing
/// definition objects must stay byte-identical.
pub const CATALOG_TYPE_NAMES: &[&str] = &[
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
];

pub const ERROR_UNAUTHORIZED: &str = "unauthorized";
pub const ERROR_INVALID_JSON: &str = "invalidJson";
pub const ERROR_MISSING_EXPECTED_REVISION: &str = "missingExpectedRevision";
pub const ERROR_REVISION_CONFLICT: &str = "revisionConflict";
pub const ERROR_INVALID_REQUEST: &str = "invalidRequest";
pub const ERROR_INTERNAL: &str = "internal";

/// Live CAS token, process generation, and pricing snapshot id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlRevision {
    pub revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
}

impl ControlRevision {
    pub fn from_state(state: &CoreState) -> Self {
        Self {
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
            pricing_revision: state.pricing_snapshot().revision.clone(),
        }
    }
}

/// Required process-scoped mutation precondition.
///
/// Both fields travel at the top level of every mutation request. The random
/// process generation prevents a revision captured before restart from being
/// accepted by a fresh process whose in-memory counter reused the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationExpectation {
    pub expected_revision: u64,
    pub process_generation: u64,
}

/// Successful control-plane mutation acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutationAck {
    pub revision: u64,
    pub process_generation: u64,
}

/// Pricing snapshot identity. Distinct from the u64 settings CAS token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PricingRevision {
    pub pricing_revision: String,
}

/// Stable non-2xx JSON envelope for every Dashboard V3 error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct V3Error {
    pub code: String,
    pub message: String,
    pub current_revision: Option<u64>,
    pub process_generation: Option<u64>,
}

impl V3Error {
    pub fn unauthorized() -> Self {
        Self {
            code: ERROR_UNAUTHORIZED.to_string(),
            message: "dashboard session is required".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn invalid_json() -> Self {
        Self {
            code: ERROR_INVALID_JSON.to_string(),
            message: "request body must be valid JSON".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn missing_expected_revision() -> Self {
        Self {
            code: ERROR_MISSING_EXPECTED_REVISION.to_string(),
            message: "expectedRevision is required".to_string(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn revision_conflict(current_revision: u64, process_generation: u64) -> Self {
        Self {
            code: ERROR_REVISION_CONFLICT.to_string(),
            message: "settings changed since they were loaded; reload and try again".to_string(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INVALID_REQUEST.to_string(),
            message: message.into(),
            current_revision: None,
            process_generation: None,
        }
    }

    pub fn invalid_request_at(
        message: impl Into<String>,
        current_revision: u64,
        process_generation: u64,
    ) -> Self {
        Self {
            code: ERROR_INVALID_REQUEST.to_string(),
            message: message.into(),
            current_revision: Some(current_revision),
            process_generation: Some(process_generation),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ERROR_INTERNAL.to_string(),
            message: message.into(),
            current_revision: None,
            process_generation: None,
        }
    }
}

/// Lightweight connection-center payload. The only V3 DTO allowed to carry
/// plaintext primary and sub Key values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionInfo {
    pub gateway_port: u16,
    pub client_root_url: String,
    pub upstream_base_url: String,
    pub primary_key: String,
    pub sub_keys: Vec<ConnectionSubKey>,
    pub revision: u64,
    pub process_generation: u64,
}

/// One non-deleted sub Key as exposed by [`ConnectionInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSubKey {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub value: String,
}

/// Application settings contract. Never contains primary/sub Key plaintext
/// or a field named `gatewayKey` / `key`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub revision: u64,
    pub process_generation: u64,
    pub gateway_port: u16,
    pub upstream_base_url: String,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    pub proxy_list_direction: ProxyListDirection,
    pub proxy_list_models: Vec<String>,
    pub proxy_supported_models: Vec<ProxySupportedModel>,
    pub opencode_invite_url: String,
    pub client_root_url: String,
    pub client_root_url_from_env: bool,
    pub auto_start: Option<bool>,
    pub auto_start_supported: bool,
    pub show_dock_icon: Option<bool>,
    pub dock_visibility_supported: bool,
    pub connect_timeout_secs: u64,
    pub non_stream_timeout_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub routing_mode: RoutingMode,
    pub conversation_sticky: bool,
}

/// PATCH-style settings write. `expectedRevision` and `processGeneration`
/// are required; every other field may be omitted. Unknown fields, including
/// any Key material, are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_mode: Option<ProxyMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_list_direction: Option<ProxyListDirection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_list_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_invite_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_root_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_start: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_dock_icon: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_stream_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_mode: Option<RoutingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_sticky: Option<bool>,
}

/// One known model backing the list-mode checkbox grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProxySupportedModel {
    pub id: String,
    pub preferred_protocol: String,
    pub zen_free: bool,
}

/// Global outbound proxy mode. Wire values stay kebab-case, matching V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum ProxyMode {
    Auto,
    Manual,
    Direct,
    List,
}

/// Which listed models take the list-mode exception leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum ProxyListDirection {
    Whitelist,
    Blacklist,
}

/// Account selection mode. Wire values stay kebab-case, matching V2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[schemars(rename_all = "kebab-case")]
pub enum RoutingMode {
    StrictPriority,
    StickyGlobal,
    RoundRobin,
}

/// Deterministic JSON Schema catalog for the checked-in V3 contract.
///
/// Response types are generated with the serialize contract so `Option` fields
/// stay required `T | null`. Request types use the deserialize contract so
/// optional fields may be omitted. Adding a DTO later must append a `$defs`
/// entry without renaming existing definitions.
pub fn contract_schema() -> Value {
    let mut serialize = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    include_type::<ControlRevision>(&mut serialize);
    include_type::<MutationAck>(&mut serialize);
    include_type::<PricingRevision>(&mut serialize);
    include_type::<V3Error>(&mut serialize);
    include_type::<ConnectionInfo>(&mut serialize);
    include_type::<ConnectionSubKey>(&mut serialize);
    include_type::<Settings>(&mut serialize);
    include_type::<ProxySupportedModel>(&mut serialize);
    let mut defs = serialize.take_definitions(true);

    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<MutationExpectation>(&mut deserialize);
    include_type::<SettingsUpdate>(&mut deserialize);
    for (name, schema) in deserialize.take_definitions(true) {
        defs.entry(name).or_insert(schema);
    }

    for name in CATALOG_TYPE_NAMES {
        if !defs.contains_key(*name) {
            panic!("dashboard v3 schema catalog is missing $defs/{name}");
        }
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "DashboardApiV3",
        "$comment": "Extensible Dashboard V3 contract catalog. Add new $defs for later DTOs; do not rename or reshape existing definitions. ConnectionInfo is the only plaintext Key DTO.",
        "anyOf": catalog_refs(&defs),
        "$defs": defs,
    })
}

/// Pretty-printed catalog JSON with a trailing newline.
pub fn contract_schema_pretty() -> String {
    let mut encoded = serde_json::to_string_pretty(&contract_schema())
        .expect("dashboard v3 schema should serialize");
    if !encoded.ends_with('\n') {
        encoded.push('\n');
    }
    encoded
}

fn include_type<T: JsonSchema>(generator: &mut SchemaGenerator) {
    generator.subschema_for::<T>();
}

fn catalog_refs(defs: &Map<String, Value>) -> Vec<Value> {
    CATALOG_TYPE_NAMES
        .iter()
        .filter(|name| defs.contains_key(**name))
        .map(|name| json!({ "$ref": format!("#/$defs/{name}") }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "pricingRevision": "seed",
            })
        );

        let parsed: MutationExpectation = serde_json::from_value(json!({
            "expectedRevision": 3,
            "processGeneration": 9,
        }))
        .unwrap();
        assert_eq!(parsed.expected_revision, 3);
        assert_eq!(parsed.process_generation, 9);
        assert!(
            serde_json::from_value::<MutationExpectation>(json!({ "expected_revision": 3 }))
                .is_err()
        );
    }

    #[test]
    fn error_envelope_always_emits_nullable_fields() {
        let error = V3Error::missing_expected_revision();
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "missingExpectedRevision");
        assert_eq!(value["currentRevision"], Value::Null);
        assert_eq!(value["processGeneration"], Value::Null);
        assert!(!value.as_object().unwrap().contains_key("current_revision"));
    }

    #[test]
    fn schema_catalog_is_extensible_and_names_kernel_types() {
        let schema = contract_schema();
        let defs = schema["$defs"].as_object().expect("catalog $defs");
        for name in CATALOG_TYPE_NAMES {
            assert!(defs.contains_key(*name), "missing {name}");
        }
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
            upstream_base_url: "https://opencode.ai/zen/go".into(),
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
            upstream_base_url: "https://opencode.ai/zen/go".into(),
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
        ] {
            assert!(
                !object.contains_key(forbidden),
                "settings must not expose {forbidden}"
            );
        }
        assert_eq!(value["autoStart"], Value::Null);
        assert_eq!(value["showDockIcon"], Value::Null);
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
                "expected_revision": 7,
                "processGeneration": 9
            }))
            .is_err()
        );
    }
}
