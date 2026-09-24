//! Read-only V4 template catalog. Built-ins plus the manual `custom-http`
//! template. Preset forms remain frontend-owned; Rust consumes only the
//! offering projection generated from the same `resources/provider-presets.json`.

use axum::Json;

use crate::dashboard_v3::{AccountAuthScheme, AccountCredentialKind, AccountUpstreamProtocol};
use crate::provider::{BUILTIN_PROVIDERS, ProviderAdapterKind, ProviderRegistry, builtin_offering};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::EndpointOperation;
use ocg_domain::ids::CPA_PROVIDER_ID;

use super::types::{EndpointSpec, OfferingKind, ProviderTemplate, TemplateList, TemplateSource};

const TEMPLATE_VERSION: u32 = 1;
const CUSTOM_HTTP_TEMPLATE_ID: &str = "custom-http";
const CONFIGURABLE_HTTP_EDITABLE_FIELDS: &[&str] = &[
    "name",
    "endpointUrl",
    "upstreamProtocol",
    "authKind",
    "models",
];

pub(super) async fn list_templates() -> Json<TemplateList> {
    Json(TemplateList {
        templates: builtin_templates(),
    })
}

pub(super) fn builtin_templates() -> Vec<ProviderTemplate> {
    let mut templates: Vec<ProviderTemplate> = BUILTIN_PROVIDERS
        .iter()
        .filter(|plan| !plan.product_surface.is_external_integration())
        .map(template_from_builtin)
        .collect();
    templates.push(custom_http_template());
    templates
}

fn template_from_builtin(plan: &crate::provider::BuiltinProvider) -> ProviderTemplate {
    let adapter = ProviderAdapterKind::from_provider_id(plan.provider_id)
        .expect("builtin catalog rows have a sealed adapter");
    ProviderTemplate {
        id: plan.provider_id.to_string(),
        version: TEMPLATE_VERSION,
        display_name: plan.display_name.to_string(),
        family_id: Some(plan.display_family.to_string()),
        offering_tags: vec![offering_kind(builtin_offering(plan.provider_id))],
        adapter_kind: adapter.as_str().to_string(),
        source: TemplateSource::Builtin,
        credential_kind: plan.credential_kind.into(),
        auth_schemes: plan
            .auth_schemes
            .iter()
            .copied()
            .map(AccountAuthScheme::from)
            .collect(),
        upstream_protocols: plan
            .upstream_protocols
            .iter()
            .copied()
            .map(AccountUpstreamProtocol::from)
            .collect(),
        editable_fields: Vec::new(),
        default_endpoints: plan
            .upstream_protocols
            .iter()
            .copied()
            .map(|protocol| EndpointSpec {
                operation: EndpointOperation::from(protocol),
                wire_protocol: AccountUpstreamProtocol::from(protocol),
                url: None,
                locked: true,
            })
            .collect(),
        pricing_multiplier_editable: ProviderRegistry::get(plan.provider_id)
            .expect("builtin catalog rows have a sealed descriptor")
            .pricing
            .multiplier_editable,
    }
}

fn custom_http_template() -> ProviderTemplate {
    ProviderTemplate {
        id: CUSTOM_HTTP_TEMPLATE_ID.to_string(),
        version: TEMPLATE_VERSION,
        display_name: "Custom HTTP".to_string(),
        family_id: None,
        offering_tags: vec![OfferingKind::Api],
        adapter_kind: ProviderAdapterKind::ConfigurableHttp.as_str().to_string(),
        source: TemplateSource::Builtin,
        credential_kind: AccountCredentialKind::ApiKey,
        auth_schemes: vec![
            AccountAuthScheme::Bearer,
            AccountAuthScheme::XApiKey,
            AccountAuthScheme::ApiKey,
        ],
        upstream_protocols: UpstreamProtocolKind::ALL
            .iter()
            .copied()
            .map(AccountUpstreamProtocol::from)
            .collect(),
        editable_fields: CONFIGURABLE_HTTP_EDITABLE_FIELDS
            .iter()
            .map(|field| (*field).to_string())
            .collect(),
        default_endpoints: UpstreamProtocolKind::ALL
            .iter()
            .copied()
            .map(|protocol| EndpointSpec {
                operation: EndpointOperation::from(protocol),
                wire_protocol: AccountUpstreamProtocol::from(protocol),
                url: None,
                locked: false,
            })
            .collect(),
        pricing_multiplier_editable: false,
    }
}

pub(super) fn offering_kind(value: &str) -> OfferingKind {
    match value {
        "plan" => OfferingKind::Plan,
        _ => OfferingKind::Api,
    }
}

pub(super) fn is_cpa_id(provider_id: &str) -> bool {
    provider_id == CPA_PROVIDER_ID
}
