use super::*;
use serde_json::json;

#[test]
fn wire_fields_are_camel_case() {
    let list = TemplateList {
        templates: vec![ProviderTemplate {
            id: "custom-http".into(),
            version: 1,
            display_name: "Custom HTTP".into(),
            family_id: None,
            offering_tags: vec![OfferingKind::Api],
            adapter_kind: "configurable_http".into(),
            source: TemplateSource::Builtin,
            credential_kind: AccountCredentialKind::ApiKey,
            auth_schemes: vec![AccountAuthScheme::Bearer],
            upstream_protocols: vec![AccountUpstreamProtocol::ChatCompletions],
            editable_fields: vec!["name".into()],
            default_endpoints: vec![EndpointSpec {
                operation: EndpointOperation::ChatCreate,
                wire_protocol: AccountUpstreamProtocol::ChatCompletions,
                url: None,
                locked: false,
            }],
        }],
    };
    let value = serde_json::to_value(&list).unwrap();
    assert_eq!(value["templates"][0]["displayName"], "Custom HTTP");
    assert_eq!(value["templates"][0]["familyId"], Value::Null);
    assert_eq!(value["templates"][0]["offeringTags"], json!(["api"]));
    assert_eq!(value["templates"][0]["adapterKind"], "configurable_http");
    assert_eq!(value["templates"][0]["credentialKind"], "api_key");
    assert_eq!(value["templates"][0]["editableFields"], json!(["name"]));
    assert_eq!(
        value["templates"][0]["defaultEndpoints"][0]["wireProtocol"],
        "chat_completions"
    );
    assert_eq!(
        value["templates"][0]["defaultEndpoints"][0]["url"],
        Value::Null
    );
}

#[test]
fn connection_summary_emits_null_optional_fields() {
    let summary = ConnectionSummary {
        id: "id".into(),
        name: "Lab".into(),
        origin: ConnectionOrigin::Custom,
        template_ref: None,
        adapter_kind: "configurable_http".into(),
        lifecycle: ConnectionLifecycle::Configured,
        authorization: AuthorizationState::Unknown,
        eligibility: Eligibility {
            state: EligibilityState::Eligible,
            reason: EligibilityReason::None,
        },
        credential_count: 1,
        enabled_credential_count: 1,
        target_count: 0,
        endpoints: Vec::new(),
        targets: Vec::new(),
        legacy: LegacyIdentity {
            kind: LegacyConnectionKind::DynamicProvider,
            id: "legacy".into(),
        },
        display_family: None,
        offering: OfferingKind::Api,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["templateRef"], Value::Null);
    assert_eq!(value["displayFamily"], Value::Null);
    assert_eq!(value["authorization"], "unknown");
    assert_eq!(value["eligibility"]["reason"], "none");
    assert_eq!(value["legacy"]["kind"], "dynamic_provider");
}

#[test]
fn schema_catalog_names_v4_types_and_shared_error() {
    let schema = contract_schema();
    assert_eq!(schema["title"], "DashboardApiV4");
    let defs = schema["$defs"].as_object().expect("catalog $defs");
    for name in CATALOG_TYPE_NAMES {
        assert!(defs.contains_key(*name), "missing $defs/{name}");
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
}
