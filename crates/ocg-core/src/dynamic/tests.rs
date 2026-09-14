use super::*;
use chrono::Utc;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
};

fn mapping(
    public_model: &str,
    upstream_model: &str,
    r#override: Option<DynamicModelUpstreamOverride>,
) -> DynamicModelMapping {
    DynamicModelMapping {
        public_model: public_model.into(),
        upstream_model: upstream_model.into(),
        upstream_override: r#override,
    }
}

fn override_route(
    protocol: UpstreamProtocolKind,
    endpoint_url: &str,
) -> DynamicModelUpstreamOverride {
    DynamicModelUpstreamOverride {
        protocol,
        endpoint_url: endpoint_url.into(),
    }
}

fn definition(mappings: Vec<DynamicModelMapping>) -> DynamicProviderDefinition {
    DynamicProviderDefinition {
        preset_id: None,
        id: "11111111-1111-1111-1111-111111111111".into(),
        name: "Lab".into(),
        endpoint_url: "http://127.0.0.1:9/v1".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings,
    }
}

#[test]
fn effective_route_prefers_mapping_override_over_runtime_defaults() {
    let inherited = mapping("chat", "vendor/chat", None);
    let overridden = mapping(
        "messages",
        "vendor/messages",
        Some(override_route(
            UpstreamProtocolKind::Messages,
            "http://127.0.0.1:9/anthropic/v1/messages",
        )),
    );
    let runtime = DynamicProviderRuntime {
        preset_id: None,
        id: "provider".into(),
        name: "Lab".into(),
        endpoint_url: "http://127.0.0.1:9/v1".into(),
        upstream_protocol: UpstreamProtocolKind::ChatCompletions,
        auth_kind: DynamicAuthKind::Bearer,
        mappings: vec![inherited.clone(), overridden.clone()],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        origin: ocg_domain::provider::ProviderOrigin::Custom,
        offering: "api".to_string(),
    };
    assert_eq!(
        runtime.effective_route(&inherited),
        DynamicEffectiveRoute {
            protocol: UpstreamProtocolKind::ChatCompletions,
            endpoint_url: "http://127.0.0.1:9/v1".into(),
        }
    );
    assert_eq!(
        runtime.effective_route(&overridden),
        DynamicEffectiveRoute {
            protocol: UpstreamProtocolKind::Messages,
            endpoint_url: "http://127.0.0.1:9/anthropic/v1/messages".into(),
        }
    );
}

#[test]
fn validate_definition_normalizes_override_endpoint() {
    let validated = validate_definition(definition(vec![mapping(
        "messages",
        "vendor/messages",
        Some(override_route(
            UpstreamProtocolKind::Messages,
            "http://127.0.0.1:9/anthropic/v1/messages/",
        )),
    )]))
    .unwrap();
    assert_eq!(
        validated.mappings[0]
            .upstream_override
            .as_ref()
            .unwrap()
            .endpoint_url,
        "http://127.0.0.1:9/anthropic/v1/messages"
    );
}

#[test]
fn validate_definition_rejects_invalid_override_endpoint() {
    let error = validate_definition(definition(vec![mapping(
        "messages",
        "vendor/messages",
        Some(override_route(UpstreamProtocolKind::Messages, "not-a-url")),
    )]))
    .unwrap_err();
    assert!(matches!(
        error,
        ProviderBindingError::InvalidCustomBaseUrl(_)
    ));
}

#[test]
fn public_name_synonyms_with_the_same_effective_route_remain_valid() {
    let validated = validate_definition(definition(vec![
        mapping("lab-opus", "vendor/opus", None),
        mapping("opus-alias", "vendor/opus", None),
        mapping(
            "opus-complete",
            "vendor/opus",
            Some(override_route(
                UpstreamProtocolKind::ChatCompletions,
                "http://127.0.0.1:9/v1/chat/completions",
            )),
        ),
    ]))
    .unwrap();
    assert_eq!(validated.mappings.len(), 3);
}

#[test]
fn same_upstream_model_with_different_effective_routes_is_rejected() {
    let error = validate_definition(definition(vec![
        mapping("lab-chat", "vendor/opus", None),
        mapping(
            "lab-messages",
            "vendor/opus",
            Some(override_route(
                UpstreamProtocolKind::Messages,
                "http://127.0.0.1:9/anthropic/v1/messages",
            )),
        ),
    ]))
    .unwrap_err();
    match error {
        ProviderBindingError::InvalidModelId(message) => {
            assert!(message.contains("vendor/opus"), "{message}");
        }
        other => panic!("expected InvalidModelId, got {other:?}"),
    }
}
