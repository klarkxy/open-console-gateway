//! Exact endpoint and per-route auth from stored routes and sealed constants.
//!
//! This does not send. A historical remote CPA base is not an upstream URL.
//! [`crate::cpa::normalize_base_url`] still accepts only the owned loopback child.

use super::types::{ProjectedRoute, ProjectionError};
use crate::custom_http::{
    ensure_sealed_secret_origin, ensure_secret_origin_granted, inspect_custom_url,
    resolve_custom_endpoints,
};
use crate::provider::builtin_provider;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{ConnectionId, EndpointOperation};
use ocg_domain::credential::{RouteSpec, assigned_endpoints_for_routes};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, LegacyDestinationRef,
    http_configured_routes, http_model_protocols, http_model_route,
};
use ocg_domain::ids::{OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH};
use ocg_domain::protocol::ApiFormat;
use ocg_domain::provider::{
    COMMAND_CODE_GOAT_BASE_URL, KIMI_CN_BASE_URL, KIMI_CN_CHAT_COMPLETIONS_PATH,
    KIMI_CN_MESSAGES_PATH, MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_BASE_URL,
    MINIMAX_CN_CHAT_COMPLETIONS_PATH, MINIMAX_CN_MESSAGES_PATH, MINIMAX_CN_RESPONSES_PATH,
    OPENCODE_GO_BASE_URL, OPENCODE_ZEN_BASE_URL,
};

pub(super) struct ResolvedModel {
    pub routes: Vec<ProjectedRoute>,
    pub needs_material: bool,
}

pub(super) fn resolve_model(
    destination: &Destination,
    model: &CatalogModel,
    connection: &ConnectionId,
    provider_id: &str,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<ResolvedModel, ProjectionError> {
    let protocols = protocol_order(destination, model);
    if protocols.is_empty() {
        return Err(ProjectionError::new(
            "invalid_endpoint",
            format!(
                "model `{}` on destination `{}` has no protocol",
                model.public_model, destination.id
            ),
        ));
    }
    let grant_routes = grant_routes(destination, provider_id);
    let assigned = assigned_endpoints_for_routes(connection, &grant_routes);
    let mut routes = Vec::with_capacity(protocols.len());
    for protocol in protocols {
        let (url, auth) = endpoint_for(destination, model, protocol, provider_id, endpoints)?;
        let url = canonical_http_url(&url).map_err(|_| {
            ProjectionError::new(
                "invalid_endpoint",
                format!(
                    "invalid endpoint for `{}` on destination `{}`",
                    protocol.as_str(),
                    destination.id
                ),
            )
        })?;
        let stored_url =
            http_model_route(destination, model, protocol).map(|route| route.endpoint_url);
        let endpoint_id =
            endpoint_id_for_protocol(&assigned, &grant_routes, protocol, stored_url.as_deref())
                .ok_or_else(|| {
                    ProjectionError::new(
                        "invalid_grant",
                        format!(
                            "no grant id for `{}` on destination `{}`",
                            protocol.as_str(),
                            destination.id
                        ),
                    )
                })?;
        routes.push(ProjectedRoute {
            protocol: protocol.as_str().to_string(),
            endpoint_url: url,
            auth: auth.as_str().to_string(),
            endpoint_id,
            validation_only: false,
        });
    }
    let needs_material = routes
        .iter()
        .any(|route| route.auth != AuthScheme::None.as_str());
    Ok(ResolvedModel {
        routes,
        needs_material,
    })
}

pub(super) fn check_route_grant(
    route: &ProjectedRoute,
    allowed_endpoint_ids: &[String],
    allowed_origins: &[String],
    sealed_base: Option<&str>,
) -> Result<(), ProjectionError> {
    if !allowed_endpoint_ids
        .iter()
        .any(|id| id == &route.endpoint_id)
    {
        return Err(ProjectionError::new(
            "invalid_grant",
            format!("endpoint grant missing for protocol `{}`", route.protocol),
        ));
    }
    if route.auth == AuthScheme::None.as_str() {
        return Ok(());
    }
    if allowed_origins.is_empty() {
        let Some(base) = sealed_base else {
            return Err(ProjectionError::new(
                "invalid_grant",
                "keyed HTTP route has no granted origin",
            ));
        };
        return ensure_sealed_secret_origin(&route.endpoint_url, base).map_err(|_| {
            ProjectionError::new("invalid_grant", "sealed endpoint origin is not granted")
        });
    }
    ensure_secret_origin_granted(&route.endpoint_url, allowed_origins)
        .map_err(|_| ProjectionError::new("invalid_grant", "endpoint origin is not granted"))
}

pub(super) fn owned_listener_origin(value: &str) -> Result<Origin, ProjectionError> {
    let normalized = crate::cpa::normalize_base_url(value, false).map_err(|_| {
        ProjectionError::new(
            "invalid_owned_listener",
            "owned CPA listener must be a loopback origin",
        )
    })?;
    origin_of(&normalized).ok_or_else(|| {
        ProjectionError::new("invalid_owned_listener", "owned CPA listener has no origin")
    })
}

pub(super) fn remote_origin(value: &str) -> Result<Origin, ProjectionError> {
    let url = canonical_http_url(value)
        .map_err(|_| ProjectionError::new("invalid_endpoint", "remote CPA base URL is invalid"))?;
    origin_of(&url).ok_or_else(|| {
        ProjectionError::new("invalid_endpoint", "remote CPA base URL has no origin")
    })
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

fn protocol_order(destination: &Destination, model: &CatalogModel) -> Vec<UpstreamProtocolKind> {
    if model.upstream_override.is_some() || model.protocols.is_empty() {
        return http_model_protocols(destination, model);
    }
    model.protocols.clone()
}

fn grant_routes(destination: &Destination, provider_id: &str) -> Vec<RouteSpec> {
    if destination.adapter != AdapterKind::Http
        && let Some(plan) = builtin_provider(provider_id)
    {
        return plan
            .upstream_protocols
            .iter()
            .copied()
            .map(|protocol| RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: None,
            })
            .collect();
    }
    http_configured_routes(destination)
}

fn endpoint_id_for_protocol(
    assigned: &[ocg_domain::credential::AssignedEndpoint],
    routes: &[RouteSpec],
    protocol: UpstreamProtocolKind,
    stored_url: Option<&str>,
) -> Option<String> {
    let operation = EndpointOperation::from(protocol);
    let mut pairs = routes.iter().zip(assigned.iter());
    if let Some(stored) = stored_url.map(str::trim).filter(|value| !value.is_empty()) {
        if let Some((_, endpoint)) = pairs
            .clone()
            .find(|(route, _)| route.operation == operation && route.url.as_deref() == Some(stored))
        {
            return Some(endpoint.id.clone());
        }
    }
    pairs
        .find(|(route, _)| route.operation == operation && route.url.is_none())
        .or_else(|| {
            routes
                .iter()
                .zip(assigned.iter())
                .find(|(route, _)| route.operation == operation)
        })
        .map(|(_, endpoint)| endpoint.id.clone())
}

fn endpoint_for(
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
    provider_id: &str,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<(String, AuthScheme), ProjectionError> {
    if destination.adapter == AdapterKind::Cpa {
        return Err(ProjectionError::new(
            "migration_required",
            "historical remote CPA is not an upstream route",
        ));
    }
    if destination.adapter == AdapterKind::Http {
        return http_endpoint(destination, model, protocol);
    }
    if let Some(route) = http_model_route(destination, model, protocol)
        && !route.endpoint_url.trim().is_empty()
        && model.upstream_override.is_some()
    {
        let url = resolve_http_url(&route.endpoint_url, protocol, destination)?;
        return Ok((url, route.auth_scheme));
    }
    sealed_endpoint(destination, protocol, provider_id, endpoints)
}

fn http_endpoint(
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
) -> Result<(String, AuthScheme), ProjectionError> {
    let route = http_model_route(destination, model, protocol)
        .ok_or_else(|| unsupported(protocol, destination))?;
    let explicit = !destination.protocol_routes.is_empty() || model.upstream_override.is_some();
    let source = if explicit {
        route.endpoint_url.as_str()
    } else {
        destination.base_url.as_deref().unwrap_or("")
    };
    let url = resolve_http_url(source, protocol, destination)?;
    let auth = if !explicit && matches!(destination.legacy, LegacyDestinationRef::CustomAccount(_))
    {
        crate::custom_http::custom_auth_scheme(protocol).into()
    } else {
        route.auth_scheme
    };
    Ok((url, auth))
}

fn resolve_http_url(
    source: &str,
    protocol: UpstreamProtocolKind,
    destination: &Destination,
) -> Result<String, ProjectionError> {
    let invalid = || {
        ProjectionError::new(
            "invalid_endpoint",
            format!(
                "invalid HTTP endpoint for `{}` on destination `{}`",
                protocol.as_str(),
                destination.id
            ),
        )
    };
    let (bare, query) = split_preserved_query(source).map_err(|_| invalid())?;
    let resolved = resolve_custom_endpoints(&bare, protocol).map_err(|_| invalid())?;
    attach_query(resolved.inference.as_str(), query.as_deref()).map_err(|_| invalid())
}

fn sealed_endpoint(
    destination: &Destination,
    protocol: UpstreamProtocolKind,
    provider_id: &str,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<(String, AuthScheme), ProjectionError> {
    let plan = builtin_provider(provider_id).ok_or_else(|| unsupported(protocol, destination))?;
    if !plan.upstream_protocols.contains(&protocol) {
        return Err(unsupported(protocol, destination));
    }
    // Product builds keep official constants. A stored loopback is not rewritten
    // onto those hosts. Acceptance of an HTTP loopback origin is the existing
    // `ollama-cloud-loopback-test` feature, and it never disables TLS checks.
    let acceptance = accept_stored_sealed_base(destination)?;
    let (url, auth) = match destination.adapter {
        AdapterKind::OpencodeGo => (
            join_url(
                OPENCODE_GO_BASE_URL,
                api_format(protocol)
                    .upstream_path()
                    .unwrap_or("/v1/chat/completions"),
            ),
            if protocol == UpstreamProtocolKind::Messages {
                AuthScheme::XApiKey
            } else {
                AuthScheme::Bearer
            },
        ),
        AdapterKind::Zen => (
            join_url(
                OPENCODE_ZEN_BASE_URL,
                api_format(protocol)
                    .upstream_path()
                    .unwrap_or("/v1/chat/completions"),
            ),
            AuthScheme::None,
        ),
        AdapterKind::Goat => {
            let path = ocg_domain::protocol::command_code_upstream_path(api_format(protocol))
                .ok_or_else(|| unsupported(protocol, destination))?;
            (
                join_url(COMMAND_CODE_GOAT_BASE_URL, path),
                AuthScheme::Bearer,
            )
        }
        AdapterKind::Minimax => match protocol {
            UpstreamProtocolKind::ChatCompletions => (
                join_url(MINIMAX_CN_BASE_URL, MINIMAX_CN_CHAT_COMPLETIONS_PATH),
                AuthScheme::Bearer,
            ),
            UpstreamProtocolKind::Responses => (
                join_url(MINIMAX_CN_BASE_URL, MINIMAX_CN_RESPONSES_PATH),
                AuthScheme::Bearer,
            ),
            UpstreamProtocolKind::Messages => (
                join_url(MINIMAX_CN_ANTHROPIC_BASE_URL, MINIMAX_CN_MESSAGES_PATH),
                AuthScheme::Bearer,
            ),
        },
        AdapterKind::Kimi => {
            let path = match protocol {
                UpstreamProtocolKind::ChatCompletions => KIMI_CN_CHAT_COMPLETIONS_PATH,
                UpstreamProtocolKind::Messages => KIMI_CN_MESSAGES_PATH,
                UpstreamProtocolKind::Responses => {
                    return Err(unsupported(protocol, destination));
                }
            };
            (join_url(KIMI_CN_BASE_URL, path), AuthScheme::Bearer)
        }
        AdapterKind::Ollama => {
            if protocol != UpstreamProtocolKind::ChatCompletions {
                return Err(unsupported(protocol, destination));
            }
            (
                join_url(OLLAMA_CLOUD_BASE_URL, OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH),
                AuthScheme::Bearer,
            )
        }
        AdapterKind::Http | AdapterKind::Cpa => {
            return Err(unsupported(protocol, destination));
        }
    };
    let url = rewrite_sealed_test_url(destination.adapter, url, endpoints)?;
    apply_sealed_acceptance(url, auth, acceptance)
}

/// Feature-off rewrite returns `Ok(None)` and the official URL stays.
fn rewrite_sealed_test_url(
    adapter: AdapterKind,
    url: String,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<String, ProjectionError> {
    let provider_id = match adapter {
        AdapterKind::OpencodeGo => ocg_domain::ids::OPENCODE_PROVIDER_ID,
        AdapterKind::Zen => ocg_domain::ids::OPENCODE_ZEN_FREE_PROVIDER_ID,
        AdapterKind::Goat => ocg_domain::ids::COMMAND_CODE_PROVIDER_ID,
        _ => return Ok(url),
    };
    match endpoints.rewrite_url(provider_id, &url) {
        Ok(Some(rewritten)) => Ok(rewritten),
        Ok(None) => Ok(url),
        Err(message) => Err(ProjectionError::new("invalid_endpoint", message)),
    }
}

/// Grant base for a sealed route: the protocol's official origin, or the
/// accepted HTTP loopback origin when that test feature is enabled.
pub(super) fn sealed_grant_base(
    destination: &Destination,
    protocol: UpstreamProtocolKind,
    endpoints: &crate::cpa_test_endpoints::EndpointAuthority,
) -> Result<String, ProjectionError> {
    let acceptance = accept_stored_sealed_base(destination)?;
    match acceptance {
        SealedAcceptance::Official => {
            let base = sealed_origin_base(destination.adapter, protocol)
                .ok_or_else(|| unsupported(protocol, destination))?;
            rewrite_sealed_test_url(destination.adapter, base.to_string(), endpoints)
        }
        #[cfg(feature = "ollama-cloud-loopback-test")]
        SealedAcceptance::Loopback { origin, .. } => Ok(origin),
    }
}

enum SealedAcceptance {
    Official,
    /// HTTP loopback origin only. Compiled with `ollama-cloud-loopback-test`.
    #[cfg(feature = "ollama-cloud-loopback-test")]
    Loopback {
        origin: String,
        query: Option<String>,
    },
}

fn accept_stored_sealed_base(
    destination: &Destination,
) -> Result<SealedAcceptance, ProjectionError> {
    let Some(stored) = destination
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(SealedAcceptance::Official);
    };
    let matches_official = [
        UpstreamProtocolKind::ChatCompletions,
        UpstreamProtocolKind::Responses,
        UpstreamProtocolKind::Messages,
    ]
    .into_iter()
    .filter_map(|protocol| sealed_origin_base(destination.adapter, protocol))
    .any(|official| crate::custom_http::origins_match(stored, official));
    if matches_official {
        return Ok(SealedAcceptance::Official);
    }
    if crate::custom_http::is_loopback_inference_origin(stored) {
        return accept_stored_loopback(stored);
    }
    Err(ProjectionError::new(
        "invalid_endpoint",
        "sealed destination base is not the official origin",
    ))
}

#[cfg(not(feature = "ollama-cloud-loopback-test"))]
fn accept_stored_loopback(_stored: &str) -> Result<SealedAcceptance, ProjectionError> {
    Err(ProjectionError::new(
        "invalid_endpoint",
        "stored loopback sealed base is refused; feature ollama-cloud-loopback-test accepts an HTTP loopback origin, and product builds do not retarget that base to the official host",
    ))
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn accept_stored_loopback(stored: &str) -> Result<SealedAcceptance, ProjectionError> {
    let parsed = reqwest::Url::parse(stored.trim()).map_err(|_| {
        ProjectionError::new("invalid_endpoint", "stored loopback sealed base is invalid")
    })?;
    inspect_custom_url(&parsed).map_err(|_| {
        ProjectionError::new("invalid_endpoint", "stored loopback sealed base is invalid")
    })?;
    // HTTP only. Accepting HTTPS loopback would be a TLS bypass of the official host.
    if parsed.scheme() != "http" || parsed.fragment().is_some() {
        return Err(ProjectionError::new(
            "invalid_endpoint",
            "stored loopback sealed base must be HTTP and must not include a fragment",
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        ProjectionError::new("invalid_endpoint", "stored loopback sealed base is invalid")
    })?;
    let host = if host.contains(':') {
        format!("[{}]", host.trim_matches(|ch: char| ch == '[' || ch == ']'))
    } else {
        host.to_string()
    };
    let port = parsed.port_or_known_default().ok_or_else(|| {
        ProjectionError::new("invalid_endpoint", "stored loopback sealed base is invalid")
    })?;
    Ok(SealedAcceptance::Loopback {
        origin: format!("http://{host}:{port}"),
        query: parsed.query().map(str::to_string),
    })
}

fn apply_sealed_acceptance(
    url: String,
    auth: AuthScheme,
    acceptance: SealedAcceptance,
) -> Result<(String, AuthScheme), ProjectionError> {
    match acceptance {
        SealedAcceptance::Official => Ok((url, auth)),
        #[cfg(feature = "ollama-cloud-loopback-test")]
        SealedAcceptance::Loopback { origin, query } => {
            retarget_loopback(&url, &origin, query.as_deref()).map(|url| (url, auth))
        }
    }
}

#[cfg(feature = "ollama-cloud-loopback-test")]
fn retarget_loopback(
    official_url: &str,
    origin: &str,
    query: Option<&str>,
) -> Result<String, ProjectionError> {
    let official = reqwest::Url::parse(official_url)
        .map_err(|_| ProjectionError::new("invalid_endpoint", "sealed endpoint is invalid"))?;
    let mut target = reqwest::Url::parse(origin).map_err(|_| {
        ProjectionError::new("invalid_endpoint", "stored loopback sealed base is invalid")
    })?;
    target.set_path(official.path());
    target.set_query(query);
    target.set_fragment(None);
    Ok(target.as_str().to_string())
}

fn api_format(protocol: UpstreamProtocolKind) -> ApiFormat {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => ApiFormat::ChatCompletions,
        UpstreamProtocolKind::Responses => ApiFormat::Responses,
        UpstreamProtocolKind::Messages => ApiFormat::Messages,
    }
}

fn join_url(base: &str, path: &str) -> String {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

fn canonical_http_url(value: &str) -> Result<String, ()> {
    let parsed = reqwest::Url::parse(value.trim()).map_err(|_| ())?;
    inspect_custom_url(&parsed).map_err(|_| ())?;
    if parsed.fragment().is_some() {
        return Err(());
    }
    Ok(parsed.as_str().to_string())
}

/// Keep a validated query. Fragment, userinfo, and non-http(s) still fail.
fn split_preserved_query(value: &str) -> Result<(String, Option<String>), ()> {
    let mut parsed = reqwest::Url::parse(value.trim()).map_err(|_| ())?;
    inspect_custom_url(&parsed).map_err(|_| ())?;
    if parsed.fragment().is_some() {
        return Err(());
    }
    let query = parsed.query().map(str::to_string);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Ok((parsed.as_str().trim_end_matches('/').to_string(), query))
}

fn attach_query(value: &str, query: Option<&str>) -> Result<String, ()> {
    let mut parsed = reqwest::Url::parse(value.trim()).map_err(|_| ())?;
    if let Some(query) = query {
        parsed.set_query(Some(query));
    }
    if parsed.fragment().is_some() {
        return Err(());
    }
    inspect_custom_url(&parsed).map_err(|_| ())?;
    Ok(parsed.as_str().to_string())
}

pub(super) fn origin_of(value: &str) -> Option<Origin> {
    let parsed = reqwest::Url::parse(value.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port_or_known_default()?;
    Some(Origin {
        scheme: parsed.scheme().to_ascii_lowercase(),
        host,
        port,
    })
}

pub(super) fn sealed_origin_base(
    adapter: AdapterKind,
    protocol: UpstreamProtocolKind,
) -> Option<&'static str> {
    match adapter {
        AdapterKind::OpencodeGo => Some(OPENCODE_GO_BASE_URL),
        AdapterKind::Zen => Some(OPENCODE_ZEN_BASE_URL),
        AdapterKind::Goat => Some(COMMAND_CODE_GOAT_BASE_URL),
        AdapterKind::Minimax => match protocol {
            UpstreamProtocolKind::Messages => Some(MINIMAX_CN_ANTHROPIC_BASE_URL),
            _ => Some(MINIMAX_CN_BASE_URL),
        },
        AdapterKind::Kimi => Some(KIMI_CN_BASE_URL),
        AdapterKind::Ollama => Some(OLLAMA_CLOUD_BASE_URL),
        AdapterKind::Http | AdapterKind::Cpa => None,
    }
}

fn unsupported(protocol: UpstreamProtocolKind, destination: &Destination) -> ProjectionError {
    ProjectionError::new(
        "unsupported_protocol",
        format!(
            "`{}` is not a route on destination `{}`",
            protocol.as_str(),
            destination.id
        ),
    )
}
