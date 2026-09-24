//! Side-effect-free route availability from a routing snapshot.
//!
//! Dashboard totals and request materialization share this judgment. It reads
//! destination, credential, and grant facts only. It does not decrypt a Key
//! or send a request, and builtin registry membership is not required.

use crate::kernel::protocol::ApiFormat;
use crate::routing_snapshot::ExecutionCredential;
use ocg_domain::credential::model_scope_allows;
use ocg_domain::destination::{CatalogModel, Destination};

pub(crate) fn endpoint_id_for_target(
    credential: &ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    upstream: ApiFormat,
) -> Result<String, String> {
    use ocg_domain::connection::{ConnectionId, EndpointOperation, endpoint_id_for};
    use ocg_domain::credential::assigned_endpoints_for_routes;
    use ocg_domain::destination::AdapterKind;
    let connection: ConnectionId =
        serde_json::from_value(serde_json::json!(credential.authorization_connection_id))
            .map_err(|error| error.to_string())?;
    if credential.authorization_connection_id.is_empty() {
        return Err("missing authorization connection identity".into());
    }
    let protocol = match upstream {
        ApiFormat::ChatCompletions => ocg_domain::destination::Protocol::ChatCompletions,
        ApiFormat::Responses => ocg_domain::destination::Protocol::Responses,
        ApiFormat::Messages => ocg_domain::destination::Protocol::Messages,
        ApiFormat::Gemini => return Err("client-only upstream protocol".into()),
    };
    if destination.adapter != AdapterKind::Http {
        return Ok(endpoint_id_for(&connection, EndpointOperation::from(protocol)).to_string());
    }
    let routes = ocg_domain::destination::http_configured_routes(destination);
    let selected = ocg_domain::destination::http_model_route(destination, model, protocol)
        .ok_or("missing configured HTTP protocol route")?;
    assigned_endpoints_for_routes(&connection, &routes)
        .into_iter()
        .zip(routes)
        .find(|(_, route)| {
            route.operation == EndpointOperation::from(protocol)
                && route.url.as_deref() == Some(selected.endpoint_url.as_str())
        })
        .map(|(assigned, _)| assigned.id)
        .ok_or_else(|| "missing persisted route grant identity".into())
}

/// True when this credential can contribute at least one enabled production
/// route on its destination. The counted unit is one inference credential,
/// the same legacy account row as `total_accounts`, not a presentation card.
pub(crate) fn credential_contributes_production_route(
    credential: &ExecutionCredential,
    destination: &Destination,
) -> bool {
    if !destination.enabled
        || !credential.enabled
        || !credential.binding_enabled
        || !credential.ready
        || credential.auth_error.is_some()
    {
        return false;
    }
    if destination.auth_scheme != ocg_domain::destination::AuthScheme::None
        && credential.key_cipher.is_empty()
    {
        return false;
    }
    destination.catalog.iter().any(|model| {
        model.enabled
            && model_in_scope(credential, model)
            && model
                .protocols
                .iter()
                .any(|protocol| protocol_is_authorized(credential, destination, model, *protocol))
    })
}

fn model_in_scope(credential: &ExecutionCredential, model: &CatalogModel) -> bool {
    model_scope_allows(&credential.scope, &model.public_model)
        || model_scope_allows(&credential.scope, &model.upstream_model)
}

pub(crate) fn protocol_is_authorized(
    credential: &ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    protocol: ocg_domain::destination::Protocol,
) -> bool {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    if destination.adapter == AdapterKind::Http {
        let Some(route) = ocg_domain::destination::http_model_route(destination, model, protocol)
        else {
            return false;
        };
        if route.auth_scheme == AuthScheme::None {
            return true;
        }
        let upstream = crate::provider_contracts::protocol_to_api(protocol);
        let Ok(endpoint_id) = endpoint_id_for_target(credential, destination, model, upstream)
        else {
            return false;
        };
        return credential
            .grants
            .allowed_endpoint_ids
            .iter()
            .any(|id| id == &endpoint_id)
            && credential
                .grants
                .allowed_origins
                .iter()
                .any(|origin| crate::custom_http::origins_match(origin, &route.endpoint_url));
    }
    if destination.adapter == AdapterKind::Cpa {
        return true;
    }
    if destination.auth_scheme == AuthScheme::None {
        return true;
    }
    let upstream = crate::provider_contracts::protocol_to_api(protocol);
    let Ok(endpoint_id) = endpoint_id_for_target(credential, destination, model, upstream) else {
        return false;
    };
    credential
        .grants
        .allowed_endpoint_ids
        .iter()
        .any(|id| id == &endpoint_id)
}
