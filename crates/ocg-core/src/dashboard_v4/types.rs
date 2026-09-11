//! Shared Dashboard V4 wire types and the JSON Schema catalog.
//!
//! V4 is a parallel read-only projection. Response objects serialize nullable
//! fields as `T | null`. Listings are secret-free. The error envelope reuses
//! the V3 DTO so clients can share one decoder.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::dashboard_v3::{
    AccountAuthScheme, AccountCredentialKind, AccountUpstreamProtocol, ControlRevision, V3Error,
};
use ocg_domain::connection::{
    AuthorizationState, ConnectionLifecycle, ConnectionOrigin, EligibilityReason, EligibilityState,
    EndpointAuthScheme, EndpointOperation, LegacyConnectionKind,
};

/// JSON Schema `$defs` names for the V4 catalog.
pub const CATALOG_TYPE_NAMES: &[&str] = &[
    "ControlRevision",
    "V3Error",
    "EndpointSpec",
    "ProviderTemplate",
    "TemplateList",
    "ConnectionEndpoint",
    "ConnectionTarget",
    "LegacyIdentity",
    "Eligibility",
    "TemplateRef",
    "ConnectionSummary",
    "ConnectionList",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointSpec {
    pub operation: EndpointOperation,
    pub wire_protocol: AccountUpstreamProtocol,
    pub url: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum TemplateSource {
    Builtin,
    Preset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OfferingKind {
    Plan,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTemplate {
    pub id: String,
    pub version: u32,
    pub display_name: String,
    pub family_id: Option<String>,
    pub offering_tags: Vec<OfferingKind>,
    pub adapter_kind: String,
    pub source: TemplateSource,
    pub credential_kind: AccountCredentialKind,
    pub auth_schemes: Vec<AccountAuthScheme>,
    pub upstream_protocols: Vec<AccountUpstreamProtocol>,
    pub editable_fields: Vec<String>,
    pub default_endpoints: Vec<EndpointSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateList {
    pub templates: Vec<ProviderTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionEndpoint {
    pub id: String,
    pub connection_id: String,
    pub operation: EndpointOperation,
    pub wire_protocol: AccountUpstreamProtocol,
    pub url: Option<String>,
    pub auth_scheme: EndpointAuthScheme,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionTarget {
    pub id: String,
    pub connection_id: String,
    pub public_name: String,
    pub upstream_model_id: String,
    pub endpoint_ids: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyIdentity {
    pub kind: LegacyConnectionKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct Eligibility {
    pub state: EligibilityState,
    pub reason: EligibilityReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateRef {
    pub id: String,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSummary {
    pub id: String,
    pub name: String,
    pub origin: ConnectionOrigin,
    pub template_ref: Option<TemplateRef>,
    pub adapter_kind: String,
    pub lifecycle: ConnectionLifecycle,
    pub authorization: AuthorizationState,
    pub eligibility: Eligibility,
    pub credential_count: u32,
    pub enabled_credential_count: u32,
    pub target_count: u32,
    pub endpoints: Vec<ConnectionEndpoint>,
    pub targets: Vec<ConnectionTarget>,
    pub legacy: LegacyIdentity,
    pub display_family: Option<String>,
    pub offering: OfferingKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionList {
    pub revision: ControlRevision,
    pub connections: Vec<ConnectionSummary>,
}

/// Deterministic JSON Schema catalog for the V4 contract.
///
/// Generator settings match V3: draft 2020-12, serialize-mode for response
/// DTOs so `Option` fields stay required `T | null`.
pub fn contract_schema() -> Value {
    let mut serialize = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    include_type::<ControlRevision>(&mut serialize);
    include_type::<V3Error>(&mut serialize);
    include_type::<EndpointSpec>(&mut serialize);
    include_type::<ProviderTemplate>(&mut serialize);
    include_type::<TemplateList>(&mut serialize);
    include_type::<ConnectionEndpoint>(&mut serialize);
    include_type::<ConnectionTarget>(&mut serialize);
    include_type::<LegacyIdentity>(&mut serialize);
    include_type::<Eligibility>(&mut serialize);
    include_type::<TemplateRef>(&mut serialize);
    include_type::<ConnectionSummary>(&mut serialize);
    include_type::<ConnectionList>(&mut serialize);
    let defs = serialize.take_definitions(true);

    for name in CATALOG_TYPE_NAMES {
        if !defs.contains_key(*name) {
            panic!("dashboard v4 schema catalog is missing $defs/{name}");
        }
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "DashboardApiV4",
        "$comment": "Extensible Dashboard V4 contract catalog. Add new $defs for later DTOs; do not rename or reshape existing definitions. Connection and template listings are secret-free.",
        "anyOf": catalog_refs(&defs),
        "$defs": defs })
}

/// Pretty-printed catalog JSON with a trailing newline.
pub fn contract_schema_pretty() -> String {
    let mut encoded = serde_json::to_string_pretty(&contract_schema())
        .expect("dashboard v4 schema should serialize");
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
mod tests;
