use super::*;
use crate::dashboard_v3::{MutationExpectation, ProviderDefinitionAuthKind};
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

#[test]
fn onboarding_commit_request_is_camel_case_and_includes_secret_in_canonical_json() {
    let request = OnboardingCommitRequest {
        expectation: MutationExpectation {
            expected_revision: 3,
            process_generation: 9,
        },
        operation_id: "11111111-1111-1111-1111-111111111111".into(),
        connection: OnboardingConnection::New(OnboardingConnectionNew {
            template_id: "custom-http".into(),
            name: "Lab".into(),
            endpoint_url: "https://lab.example/v1/chat/completions".into(),
            upstream_protocol: AccountUpstreamProtocol::ChatCompletions,
            auth_kind: ProviderDefinitionAuthKind::Bearer,
        }),
        authorization: Some(OnboardingAuthorization::ApiKey(
            OnboardingAuthorizationApiKey {
                secret_input: "sk-canonical".into(),
                account_label: Some("Primary".into()),
                notes: None,
            },
        )),
        targets: vec![OnboardingTarget {
            public_model: "lab-opus".into(),
            upstream_model: "vendor/opus".into(),
            upstream_override: None,
        }],
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["expectedRevision"], 3);
    assert_eq!(value["processGeneration"], 9);
    assert_eq!(value["operationId"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(value["connection"]["kind"], "new");
    assert_eq!(value["connection"]["templateId"], "custom-http");
    assert_eq!(
        value["connection"]["endpointUrl"],
        "https://lab.example/v1/chat/completions"
    );
    assert_eq!(value["authorization"]["kind"], "api_key");
    assert_eq!(value["authorization"]["secretInput"], "sk-canonical");
    assert_eq!(value["authorization"]["accountLabel"], "Primary");
    assert_eq!(value["authorization"]["notes"], Value::Null);
    assert_eq!(value["targets"][0]["publicModel"], "lab-opus");
    assert_eq!(value["targets"][0]["upstreamOverride"], Value::Null);
    let result = OnboardingCommitResult {
        revision: ControlRevision {
            revision: 4,
            process_generation: 9,
            pricing_revision: "p".into(),
        },
        connection_id: "conn".into(),
        credential_id: None,
        target_ids: vec![],
        replayed: false,
    };
    let result_value = serde_json::to_value(&result).unwrap();
    assert_eq!(result_value["connectionId"], "conn");
    assert_eq!(result_value["credentialId"], Value::Null);
    assert_eq!(result_value["targetIds"], json!([]));
    assert_eq!(result_value["replayed"], false);
}
