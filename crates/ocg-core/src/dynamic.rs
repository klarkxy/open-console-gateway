//! Host runtime for persisted dynamic Provider definitions.
//!
//! Adapter identity is always the sealed Configurable HTTP adapter. Custom API
//! remains account-owned and is not a dynamic Provider.

use chrono::{DateTime, Utc};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::dynamic::{
    DynamicAuthKind, DynamicModelMapping, DynamicModelUpstreamOverride, DynamicProviderDefinition,
    normalize_dynamic_mappings, normalize_dynamic_provider_name,
};
use ocg_domain::provider::{
    BUILTIN_PROVIDERS, ProviderAdapterKind, ProviderBindingError, ProviderOrigin,
};
use std::collections::HashMap;

use crate::custom::validate_custom_endpoint_url;
use crate::custom_http::resolve_custom_endpoints;

pub use ocg_domain::dynamic::provider_ids_equal;

/// Effective inference route for one mapping: explicit override, else Provider defaults.
/// Authentication stays on the Provider; protocol never selects an auth scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicEffectiveRoute {
    pub protocol: UpstreamProtocolKind,
    pub endpoint_url: String,
}

/// Resolve the mapping's configured route. Override wins; otherwise inherit runtime defaults.
pub fn effective_mapping_route(
    default_protocol: UpstreamProtocolKind,
    default_endpoint: &str,
    mapping: &DynamicModelMapping,
) -> DynamicEffectiveRoute {
    match &mapping.upstream_override {
        Some(value) => DynamicEffectiveRoute {
            protocol: value.protocol,
            endpoint_url: value.endpoint_url.clone(),
        },
        None => DynamicEffectiveRoute {
            protocol: default_protocol,
            endpoint_url: default_endpoint.to_string(),
        },
    }
}

/// Frozen routing view of one dynamic Provider. The runtime mirrors a single
/// row in the unified `providers` table; builtin rows reuse the same shape with
/// `origin = ProviderOrigin::Builtin` and `endpoint_url` / `upstream_protocol`
/// / `auth_kind` / `mappings` left at their adapter-defined defaults (the
/// sealed adapter drives those at call time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicProviderRuntime {
    pub preset_id: Option<String>,
    pub id: String,
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind,
    pub auth_kind: DynamicAuthKind,
    pub mappings: Vec<DynamicModelMapping>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Row provenance in the unified `providers` table.
    pub origin: ProviderOrigin,
    /// Plan/api offering label persisted alongside the row. Builtin rows
    /// carry the catalog default; dynamic rows follow `preset_id`.
    pub offering: String,
}

impl DynamicProviderRuntime {
    pub fn definition(&self) -> DynamicProviderDefinition {
        DynamicProviderDefinition {
            preset_id: self.preset_id.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            endpoint_url: self.endpoint_url.clone(),
            upstream_protocol: self.upstream_protocol,
            auth_kind: self.auth_kind,
            mappings: self.mappings.clone(),
        }
    }

    pub fn alias_catalog(&self) -> crate::alias::ExtraProviderCatalog {
        crate::alias::ExtraProviderCatalog {
            provider_id: self.id.clone(),
            mappings: self
                .mappings
                .iter()
                .map(|mapping| (mapping.public_model.clone(), mapping.upstream_model.clone()))
                .collect(),
        }
    }

    pub fn mapping_for_public(&self, requested: &str) -> Option<&DynamicModelMapping> {
        self.mappings.iter().find(|mapping| {
            crate::custom::custom_model_id_matches(&mapping.public_model, requested)
        })
    }

    pub fn mapping_for_upstream(&self, requested: &str) -> Option<&DynamicModelMapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.upstream_model.trim() == requested.trim())
    }

    pub fn effective_route(&self, mapping: &DynamicModelMapping) -> DynamicEffectiveRoute {
        effective_mapping_route(self.upstream_protocol, &self.endpoint_url, mapping)
    }
}

pub fn find_runtime<'a>(
    runtimes: &'a [DynamicProviderRuntime],
    provider_id: &str,
) -> Option<&'a DynamicProviderRuntime> {
    runtimes
        .iter()
        .find(|runtime| provider_ids_equal(&runtime.id, provider_id))
}

pub fn adapter_kind_for(
    provider_id: &str,
    runtimes: &[DynamicProviderRuntime],
) -> Option<ProviderAdapterKind> {
    if let Some(kind) = ProviderAdapterKind::from_provider_id(provider_id) {
        return Some(kind);
    }
    find_runtime(runtimes, provider_id).map(|_| ProviderAdapterKind::ConfigurableHttp)
}

pub fn collides_with_known_id(provider_id: &str, runtimes: &[DynamicProviderRuntime]) -> bool {
    BUILTIN_PROVIDERS
        .iter()
        .any(|plan| provider_ids_equal(plan.provider_id, provider_id))
        || find_runtime(runtimes, provider_id).is_some()
}

pub fn validate_definition(
    definition: DynamicProviderDefinition,
) -> Result<DynamicProviderDefinition, ProviderBindingError> {
    let name = normalize_dynamic_provider_name(&definition.name)?;
    let mappings = normalize_mapping_overrides(
        &definition.endpoint_url,
        definition.upstream_protocol,
        normalize_dynamic_mappings(&definition.mappings)?,
    )?;
    Ok(DynamicProviderDefinition {
        preset_id: normalize_preset_id(definition.preset_id)?,
        id: definition.id,
        name,
        endpoint_url: definition.endpoint_url,
        upstream_protocol: definition.upstream_protocol,
        auth_kind: definition.auth_kind,
        mappings,
    })
}

fn normalize_mapping_overrides(
    default_endpoint: &str,
    default_protocol: UpstreamProtocolKind,
    mappings: Vec<DynamicModelMapping>,
) -> Result<Vec<DynamicModelMapping>, ProviderBindingError> {
    let mut normalized = Vec::with_capacity(mappings.len());
    for mapping in mappings {
        let upstream_override = match mapping.upstream_override {
            Some(value) => Some(DynamicModelUpstreamOverride {
                protocol: value.protocol,
                endpoint_url: validate_custom_endpoint_url(&value.endpoint_url)?,
            }),
            None => None,
        };
        normalized.push(DynamicModelMapping {
            public_model: mapping.public_model,
            upstream_model: mapping.upstream_model,
            upstream_override,
        });
    }
    reject_conflicting_upstream_routes(default_endpoint, default_protocol, &normalized)?;
    Ok(normalized)
}

fn reject_conflicting_upstream_routes(
    default_endpoint: &str,
    default_protocol: UpstreamProtocolKind,
    mappings: &[DynamicModelMapping],
) -> Result<(), ProviderBindingError> {
    let mut seen = HashMap::<&str, DynamicEffectiveRoute>::new();
    for mapping in mappings {
        let route = effective_mapping_route(default_protocol, default_endpoint, mapping);
        if let Some(existing) = seen.get(mapping.upstream_model.as_str())
            && routes_conflict(existing, &route)?
        {
            return Err(ProviderBindingError::InvalidModelId(format!(
                "conflicting mappings for upstream model `{}`",
                mapping.upstream_model
            )));
        }
        seen.entry(mapping.upstream_model.as_str()).or_insert(route);
    }
    Ok(())
}

fn routes_conflict(
    left: &DynamicEffectiveRoute,
    right: &DynamicEffectiveRoute,
) -> Result<bool, ProviderBindingError> {
    if left.protocol != right.protocol {
        return Ok(true);
    }
    if left.endpoint_url == right.endpoint_url {
        return Ok(false);
    }
    Ok(resolved_inference_url(left)? != resolved_inference_url(right)?)
}

fn resolved_inference_url(route: &DynamicEffectiveRoute) -> Result<String, ProviderBindingError> {
    resolve_custom_endpoints(&route.endpoint_url, route.protocol)
        .map(|resolved| resolved.inference.as_str().to_string())
        .map_err(|error| ProviderBindingError::InvalidCustomBaseUrl(error.to_string()))
}

pub fn normalize_preset_id(value: Option<String>) -> Result<Option<String>, ProviderBindingError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 80
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ProviderBindingError::InvalidProviderName(
            "invalid preset id".to_string(),
        ));
    }
    Ok(Some(value.to_string()))
}

#[cfg(test)]
mod tests;
