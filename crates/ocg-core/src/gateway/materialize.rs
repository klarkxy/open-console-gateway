//! Materializes frozen destination/catalog targets into request and transport
//! plans. Model resolution, protocol selection, endpoint and credential grants
//! consume explicit persisted facts.

use crate::alias::{ProviderMapping, ResolveError, ResolvedModel};
use crate::gateway::protocol::{
    CustomRouteSpec, MaterializeSpec, ParsedClientRequest, ProtocolError, RequestPlan,
    materialize_parsed_request,
};
use crate::gateway::provider_adapter;
use crate::gateway::routing::RoutingCandidate;
use crate::kernel::ids::{custom_model_id_matches, normalize_model_name};
use crate::kernel::protocol::ApiFormat;
use crate::models::{AppConfig, UpstreamChannel};
use crate::provider::ProviderAdapterKind;
use axum::http::StatusCode;
use ocg_domain::credential::{ModelScope, model_scope_allows};
use ocg_domain::destination::Destination;

pub use crate::gateway::protocol::{
    parse_client_request as parse_client, parse_gemini_request as parse_gemini,
};

/// Stable internal rejection identity. Wire/explain codes are `as_str()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteRejectionCode {
    MappingProtocolIncompatible,
    CredentialDisabled,
    BindingDisabled,
    ModelScopeDenied,
    #[allow(dead_code)] // Historical explanation code retained for compatibility fixtures.
    GoatNotEligible,
    #[allow(dead_code)] // Historical explanation code retained for compatibility fixtures.
    GoatUnverified,
    CandidateMaterializationFailed,
    ProductionRouteUnsupported,
}

impl RouteRejectionCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MappingProtocolIncompatible => "mapping_protocol_incompatible",
            Self::CredentialDisabled => "credential_disabled",
            Self::BindingDisabled => "binding_disabled",
            Self::ModelScopeDenied => "model_scope_denied",
            Self::GoatNotEligible => "goat_not_eligible",
            Self::GoatUnverified => "goat_unverified",
            Self::CandidateMaterializationFailed => "candidate_materialization_failed",
            Self::ProductionRouteUnsupported => "production_route_unsupported",
        }
    }
}

/// Typed materialize rejection with the historical human detail string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteRejection {
    pub code: RouteRejectionCode,
    pub detail: String,
    pub account_id: Option<String>,
    pub provider_id: Option<String>,
    pub upstream_model: Option<String>,
}

pub(crate) fn binding_allows_requested_model(
    scope: &ModelScope,
    client_model: &str,
    routing_model: &str,
    plan_models: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    model_scope_allows(scope, routing_model)
        || (client_model != routing_model && model_scope_allows(scope, client_model))
        || plan_models
            .into_iter()
            .any(|model| model_scope_allows(scope, model.as_ref()))
}

pub(crate) fn mapping_adapter_kind(mapping: &ProviderMapping) -> Option<ProviderAdapterKind> {
    crate::dynamic::adapter_kind_for(&mapping.provider_id, &[]).or_else(|| {
        uuid::Uuid::parse_str(&mapping.provider_id)
            .ok()
            .map(|_| ProviderAdapterKind::ConfigurableHttp)
    })
}

/// Custom/platform catalog row: Configurable HTTP whose `provider_id` is the
/// sealed custom catalog key, not a dynamic UUID.
pub(crate) fn mapping_is_custom_http_catalog(mapping: &ProviderMapping) -> bool {
    mapping_adapter_kind(mapping) == Some(ProviderAdapterKind::ConfigurableHttp)
        && crate::dynamic::adapter_kind_for(&mapping.provider_id, &[])
            == Some(ProviderAdapterKind::ConfigurableHttp)
}

pub(crate) fn protocol_error_from_resolve(error: ResolveError) -> ProtocolError {
    match error.code() {
        Some(code) => ProtocolError::with_code(StatusCode::BAD_REQUEST, code, error.message()),
        None => ProtocolError::new(error.message()),
    }
}

/// Canonical registry alias persisted on forward logs for this resolution.
pub(crate) fn resolved_alias_from_model(resolved: &ResolvedModel) -> Option<String> {
    match resolved {
        ResolvedModel::Alias { alias, .. } => Some((*alias).to_string()),
        ResolvedModel::PinnedRaw { mapping, .. } => registry_alias_for_mapping(mapping),
    }
}

/// Registry alias for a unique raw mapping, when one is published.
pub(crate) fn registry_alias_for_mapping(mapping: &ProviderMapping) -> Option<String> {
    for published in crate::alias::published_aliases() {
        match crate::alias::resolve(&published) {
            Ok(ResolvedModel::Alias {
                alias, mappings, ..
            }) => {
                if mappings.iter().any(|candidate| {
                    candidate.provider_id == mapping.provider_id
                        && candidate.upstream_model == mapping.upstream_model
                }) {
                    return Some(alias.to_string());
                }
            }
            Ok(ResolvedModel::PinnedRaw { .. }) | Err(_) => {}
        }
    }
    None
}

/// Request / alias / upstream identity persisted on every forward log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeLogIdentity {
    pub requested_model: String,
    pub resolved_alias: Option<String>,
    pub upstream_model: String,
}

/// Carry materialization identity into logs without inferring at the DB layer.
pub(crate) fn native_log_identity(plan: &RequestPlan) -> NativeLogIdentity {
    let requested_model = plan.log_requested_model().to_string();
    let upstream_model = plan.log_upstream_model().to_string();
    let resolved_alias = plan
        .resolved_alias
        .clone()
        .filter(|alias| !alias.is_empty())
        .or_else(|| resolved_alias_for_name(&requested_model))
        .or_else(|| {
            plan.original_model
                .as_deref()
                .and_then(resolved_alias_for_name)
        })
        .or_else(|| resolved_alias_for_name(&upstream_model));
    NativeLogIdentity {
        requested_model,
        resolved_alias,
        upstream_model,
    }
}

pub(crate) fn resolved_alias_for_name(name: &str) -> Option<String> {
    match crate::alias::resolve(name) {
        Ok(resolved) => resolved_alias_from_model(&resolved),
        Err(_) => None,
    }
}

/// Preserve original casing when the client name already identifies this mapping.
pub(crate) fn upstream_model_for(requested: &str, canonical: &str) -> String {
    if normalize_model_name(requested) == normalize_model_name(canonical) {
        requested.to_string()
    } else {
        canonical.to_string()
    }
}

/// The persisted model selected at entry. Authorization compares this identity;
/// it never selects a different row after an edit.
#[derive(Clone, Debug)]
pub(crate) struct FrozenTarget {
    pub destination: Destination,
    pub model: ocg_domain::destination::CatalogModel,
    pub endpoint_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionRoute {
    pub routing: RoutingCandidate<crate::routing_snapshot::ExecutionCredential>,
    pub plan: RequestPlan,
    pub spec: crate::gateway::attempt::AttemptSpec,
    pub target: FrozenTarget,
}

pub(crate) struct ExecutionRouteSet {
    pub routes: Vec<ExecutionRoute>,
    pub free_only: bool,
    pub incompatibility: Option<String>,
    pub rejections: Vec<RouteRejection>,
}

pub(crate) fn resolved_contains_model(
    resolved: &ResolvedModel,
    destination: &Destination,
    model: &ocg_domain::destination::CatalogModel,
    requested: &str,
) -> bool {
    use ocg_domain::destination::{AdapterKind, ModelResolution};
    let mapping_matches = |mapping: &ProviderMapping| {
        if !mapping.routeable {
            return false;
        }
        if destination.adapter == AdapterKind::Http {
            match destination.model_resolution {
                ModelResolution::PublicOnly => {
                    mapping.provider_id == crate::provider::CUSTOM_PROVIDER_ID
                        && custom_model_id_matches(&model.public_model, requested)
                }
                ModelResolution::PublicAndUpstream => {
                    let requested_public = destination
                        .catalog
                        .iter()
                        .find(|row| custom_model_id_matches(&row.public_model, requested));
                    mapping.provider_id == destination.id
                        && mapping.upstream_model == model.upstream_model
                        && requested_public.is_none_or(|row| row.public_model == model.public_model)
                }
                ModelResolution::AdapterDefined => false,
            }
        } else {
            crate::provider::ProviderRegistry::get_by_kind(ProviderAdapterKind::from(
                destination.adapter,
            ))
            .is_some_and(|descriptor| descriptor.provider_id == mapping.provider_id)
                && normalize_model_name(&mapping.upstream_model)
                    == normalize_model_name(&model.upstream_model)
        }
    };
    match resolved {
        ResolvedModel::Alias { mappings, .. } => mappings.iter().any(mapping_matches),
        ResolvedModel::PinnedRaw { mapping, .. } => mapping_matches(mapping),
    }
}

pub(crate) use crate::route_availability::endpoint_id_for_target;

/// Protocols this Key may send on: declared and enabled, with a configured
/// route, and granted to the credential. Selection then prefers the client
/// protocol, the saved preference, and the remaining granted protocols.
fn authorized_model_protocols(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &Destination,
    model: &ocg_domain::destination::CatalogModel,
) -> Vec<ocg_domain::destination::Protocol> {
    model
        .protocols
        .iter()
        .copied()
        .filter(|protocol| {
            crate::route_availability::protocol_is_authorized(
                credential,
                destination,
                model,
                *protocol,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_execution_routes(
    snapshot: &crate::routing_snapshot::RoutingSnapshot,
    config: &AppConfig,
    parsed: &ParsedClientRequest,
    resolved: &ResolvedModel,
    client_model: &str,
    routing_model: &str,
    cpa_base_url: Option<&str>,
) -> Result<ExecutionRouteSet, ProtocolError> {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    let mut routes = Vec::new();
    let mut rejections = Vec::new();
    let mut conversion_error = None;
    let mut conversion_failures = 0;
    for credential in &snapshot.credentials {
        let Some(destination) = snapshot
            .projection
            .destinations
            .iter()
            .find(|d| d.id == credential.destination_id)
        else {
            continue;
        };
        let reject = |code, detail: String| RouteRejection {
            code,
            detail,
            account_id: Some(credential.id.clone()),
            provider_id: Some(credential.provider_id.clone()),
            upstream_model: None,
        };
        if !destination
            .catalog
            .iter()
            .any(|m| resolved_contains_model(resolved, destination, m, routing_model))
        {
            continue;
        }
        if !destination.enabled || !credential.enabled {
            rejections.push(reject(
                RouteRejectionCode::CredentialDisabled,
                "credential or destination disabled".into(),
            ));
            continue;
        }
        if !credential.binding_enabled {
            rejections.push(reject(
                RouteRejectionCode::BindingDisabled,
                "inference binding disabled".into(),
            ));
            continue;
        }
        for model in &destination.catalog {
            if !resolved_contains_model(resolved, destination, model, routing_model) {
                continue;
            }
            if !model.enabled || model.protocols.is_empty() {
                rejections.push(reject(
                    RouteRejectionCode::MappingProtocolIncompatible,
                    "model has no enabled protocol".into(),
                ));
                continue;
            }
            if !binding_allows_requested_model(
                &credential.scope,
                client_model,
                routing_model,
                [&model.upstream_model, &model.public_model],
            ) {
                rejections.push(reject(
                    RouteRejectionCode::ModelScopeDenied,
                    "model is outside credential scope".into(),
                ));
                continue;
            }
            if destination.auth_scheme != AuthScheme::None && credential.key_cipher.is_empty() {
                rejections.push(reject(
                    RouteRejectionCode::CandidateMaterializationFailed,
                    "credential has no Key".into(),
                ));
                continue;
            }
            let authorized = authorized_model_protocols(credential, destination, model);
            if authorized.is_empty() {
                rejections.push(reject(
                    RouteRejectionCode::ProductionRouteUnsupported,
                    "no granted protocol route for this Key".into(),
                ));
                continue;
            }
            let preferred = model
                .preferred
                .or_else(|| authorized.first().copied())
                .ok_or_else(|| ProtocolError::new("missing preferred protocol"))?;
            let upstream = match crate::provider_contracts::select_enabled_upstream(
                parsed.client,
                preferred,
                &authorized,
                &authorized,
            ) {
                Ok(upstream) => upstream,
                Err(error) => {
                    rejections.push(reject(
                        RouteRejectionCode::MappingProtocolIncompatible,
                        error.message,
                    ));
                    continue;
                }
            };
            let adapter = ProviderAdapterKind::from(destination.adapter);
            let channel = crate::routing_runtime::channel_for_adapter(adapter);
            let custom_route = if destination.adapter == AdapterKind::Http {
                let selected_protocol = match upstream {
                    ApiFormat::ChatCompletions => {
                        ocg_domain::destination::Protocol::ChatCompletions
                    }
                    ApiFormat::Responses => ocg_domain::destination::Protocol::Responses,
                    ApiFormat::Messages => ocg_domain::destination::Protocol::Messages,
                    ApiFormat::Gemini => {
                        return Err(ProtocolError::new("client-only upstream protocol"));
                    }
                };
                let Some(selected) = ocg_domain::destination::http_model_route(
                    destination,
                    model,
                    selected_protocol,
                ) else {
                    rejections.push(reject(
                        RouteRejectionCode::ProductionRouteUnsupported,
                        "model protocol has no configured route".into(),
                    ));
                    continue;
                };
                Some(CustomRouteSpec {
                    endpoint_url: selected.endpoint_url,
                    auth_kind: match selected.auth_scheme {
                        AuthScheme::Bearer => ocg_domain::dynamic::DynamicAuthKind::Bearer,
                        AuthScheme::XApiKey => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
                        AuthScheme::ApiKey => ocg_domain::dynamic::DynamicAuthKind::ApiKey,
                        AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
                    },
                })
            } else {
                None
            };
            let plan = materialize_parsed_request(
                parsed,
                &MaterializeSpec {
                    client_model: client_model.into(),
                    upstream_model: if destination.adapter == AdapterKind::Http {
                        model.upstream_model.clone()
                    } else {
                        upstream_model_for(routing_model, &model.upstream_model)
                    },
                    resolved_alias: resolved_alias_from_model(resolved),
                    channel,
                    upstream_base_override: if destination.adapter == AdapterKind::Cpa {
                        cpa_base_url
                            .map(str::to_string)
                            .or_else(|| destination.base_url.clone())
                    } else {
                        None
                    },
                    original_model: None,
                    forced_upstream: Some(upstream),
                    custom_route,
                    effort_aliases: crate::gateway::protocol::route_effort_aliases(
                        destination.adapter,
                        &model.upstream_model,
                    ),
                },
            );
            let plan = match plan {
                Ok(plan) => plan,
                Err(error) => {
                    conversion_failures += 1;
                    conversion_error.get_or_insert_with(|| error.clone());
                    rejections.push(reject(
                        RouteRejectionCode::CandidateMaterializationFailed,
                        error.message,
                    ));
                    continue;
                }
            };
            let spec = match provider_adapter::resolve_execution_route(
                credential,
                destination,
                config,
                &plan,
            ) {
                Ok(spec) => spec,
                Err(error) => {
                    rejections.push(reject(
                        RouteRejectionCode::ProductionRouteUnsupported,
                        error,
                    ));
                    continue;
                }
            };
            let endpoint_id = endpoint_id_for_target(credential, destination, model, upstream)
                .map_err(ProtocolError::new)?;
            routes.push(ExecutionRoute {
                routing: RoutingCandidate {
                    account: credential.clone(),
                    adapter,
                    channel,
                    resolved_model: model.upstream_model.clone(),
                },
                plan,
                spec,
                target: FrozenTarget {
                    destination: destination.clone(),
                    model: model.clone(),
                    endpoint_id,
                },
            });
            break;
        }
    }
    // Only real candidate conversions can reject client features. Unavailable
    // credentials still produce availability errors, and any viable route wins.
    if routes.is_empty()
        && conversion_failures
            + rejections
                .iter()
                .filter(|rejection| {
                    rejection.code == RouteRejectionCode::MappingProtocolIncompatible
                })
                .count()
            == rejections.len()
        && let Some(error) = conversion_error
    {
        return Err(error);
    }
    let free_only = !routes.is_empty()
        && routes
            .iter()
            .all(|r| r.routing.channel == UpstreamChannel::Free);
    let incompatibility = (routes.is_empty() && !rejections.is_empty()).then(|| {
        rejections
            .iter()
            .map(|r| r.detail.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    });
    Ok(ExecutionRouteSet {
        routes,
        free_only,
        incompatibility,
        rejections,
    })
}

#[cfg(test)]
mod tests;
