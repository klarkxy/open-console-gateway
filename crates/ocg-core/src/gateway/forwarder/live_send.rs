//! Live send authorization for a captured selection.
//!
//! Selection records binding id, credential version, and cipher identity at
//! account pick. Host secret resolution re-reads account, binding, scope, and
//! stored grants under one DB lock and decrypts only while that lock is held.
//! Same-account retry reuses the original capture. Callers re-check after
//! header/transport prep and before the outbound send.

use crate::custom_http::{
    OriginGrantError, ensure_sealed_secret_origin, ensure_secret_origin_granted, origins_match,
    resolve_custom_endpoints,
};
use crate::db::identity::StoredInferenceBinding;
use crate::dynamic::find_runtime;
use crate::gateway::attempt::{AttemptSpec, CredentialHandle, ProxyRoutingModel, UpstreamAuth};
use crate::gateway::materialize::binding_allows_requested_model;
use crate::gateway::protocol::RequestPlan;
use crate::models::Account;
use crate::provider_contracts::protocol_to_api;
use crate::state::CoreState;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    AssignedEndpoint, RouteSpec, assigned_endpoints_for_routes, normalize_origin,
};
use std::collections::HashSet;
use std::fmt;

const UNAUTHORIZED_ATTEMPT: &str =
    "refusing to send credentials: selected credential is no longer authorized for this attempt";

/// Whether a disabled account card may still decrypt and send.
///
/// The account switch is the routing draft/live gate. Operational model
/// tests and protocol probes must keep working on a disabled card so the
/// operator can check the Key before turning the card on. Binding enablement,
/// version, scope, and destination grants still apply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveSendAccountGate {
    RequireEnabled,
    AllowDisabled,
}
const MISSING_GRANT: &str = "refusing to send credentials: no persisted origin grant for this Key";
const ENDPOINT_NOT_GRANTED: &str =
    "refusing to send credentials: the endpoint is not authorized for this Key";
const ROUTE_CHANGED: &str =
    "refusing to send credentials: destination is not the current granted route";

#[derive(Debug, Clone)]
pub(crate) struct LiveSendSelection {
    pub account_id: String,
    pub binding_id: String,
    pub credential_version: u64,
    pub key_cipher: String,
    pub client_model: String,
    pub routing_model: String,
    pub plan_model: String,
}

impl LiveSendSelection {
    pub(crate) fn from_binding(
        account: &Account,
        binding: Option<&StoredInferenceBinding>,
        client_model: &str,
        routing_model: &str,
        plan_model: &str,
    ) -> Self {
        Self {
            account_id: account.id.clone(),
            binding_id: binding
                .map(|row| row.binding_id.clone())
                .unwrap_or_default(),
            credential_version: binding.map(|row| row.credential_version).unwrap_or(0),
            key_cipher: account.key_cipher.clone(),
            client_model: client_model.to_string(),
            routing_model: routing_model.to_string(),
            plan_model: plan_model.to_string(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum LiveSendAuthError {
    Unauthorized(String),
    Decrypt(anyhow::Error),
}

impl LiveSendAuthError {
    fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized(message.into())
    }

    pub(crate) fn is_decrypt(&self) -> bool {
        matches!(self, Self::Decrypt(_))
    }
}

impl fmt::Display for LiveSendAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized(message) => f.write_str(message),
            Self::Decrypt(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for LiveSendAuthError {}

impl From<OriginGrantError> for LiveSendAuthError {
    fn from(error: OriginGrantError) -> Self {
        Self::unauthorized(error.to_string())
    }
}

/// Re-read live account/binding/grants and decrypt the captured cipher in the
/// same DB read lock. Never substitutes a rotated Key into this attempt.
pub(crate) fn authorize_live_send_secret(
    state: &CoreState,
    selection: &LiveSendSelection,
    account: &Account,
    plan: &RequestPlan,
    spec: &AttemptSpec,
    account_gate: LiveSendAccountGate,
) -> Result<Option<String>, LiveSendAuthError> {
    match &spec.credential {
        CredentialHandle::None => Ok(None),
        CredentialHandle::Account { id } => {
            if id != &selection.account_id || id != &account.id {
                return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
            }
            let db = state.db.lock();
            verify_live_send(&db, selection, plan, spec, account_gate)?;
            state
                .decrypt_key(&selection.key_cipher)
                .map(Some)
                .map_err(LiveSendAuthError::Decrypt)
        }
    }
}

/// Repeat live validation after header/transport prep and before dispatch.
/// Does not decrypt; the captured cipher must still match live state.
pub(crate) fn confirm_live_send_secret(
    state: &CoreState,
    selection: &LiveSendSelection,
    account: &Account,
    plan: &RequestPlan,
    spec: &AttemptSpec,
    account_gate: LiveSendAccountGate,
) -> Result<(), LiveSendAuthError> {
    match &spec.credential {
        CredentialHandle::None => Ok(()),
        CredentialHandle::Account { id } => {
            if id != &selection.account_id || id != &account.id {
                return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
            }
            let db = state.db.lock();
            verify_live_send(&db, selection, plan, spec, account_gate)
        }
    }
}

fn live_key_cipher(
    db: &crate::db::Database,
    account: &Account,
) -> Result<String, LiveSendAuthError> {
    match db.credential_key_cipher_for_legacy_account(&account.id) {
        Ok(Some(cipher)) => Ok(cipher),
        Ok(None) => Ok(account.key_cipher.clone()),
        Err(error) => Err(LiveSendAuthError::unauthorized(error.to_string())),
    }
}

fn verify_live_send(
    db: &crate::db::Database,
    selection: &LiveSendSelection,
    plan: &RequestPlan,
    spec: &AttemptSpec,
    account_gate: LiveSendAccountGate,
) -> Result<(), LiveSendAuthError> {
    let target_url = spec
        .request_url()
        .map_err(LiveSendAuthError::unauthorized)?;
    if spec.is_local_external_integration() {
        if let Some(live_account) = db
            .get_account(&selection.account_id)
            .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
            && ((account_gate == LiveSendAccountGate::RequireEnabled && !live_account.enabled)
                || live_key_cipher(db, &live_account)? != selection.key_cipher)
        {
            return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
        }
        ensure_sealed_secret_origin(&target_url, &spec.base_url)?;
        return Ok(());
    }

    let live_account = db
        .get_account(&selection.account_id)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
        .ok_or_else(|| LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT))?;
    if account_gate == LiveSendAccountGate::RequireEnabled && !live_account.enabled {
        return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
    }
    let binding = db
        .list_inference_bindings()
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
        .into_iter()
        .find(|row| row.account_id == selection.account_id)
        .ok_or_else(|| LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT))?;
    if binding.binding_id != selection.binding_id
        || selection.binding_id.is_empty()
        || binding.credential_version != selection.credential_version
        || live_key_cipher(db, &live_account)? != selection.key_cipher
        || !binding.enabled
        || !binding_allows_requested_model(
            &binding.model_scope,
            &selection.client_model,
            &selection.routing_model,
            std::iter::once(selection.plan_model.as_str()),
        )
    {
        return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
    }

    if matches!(spec.proxy_routing, ProxyRoutingModel::IsolatedTrustedAdmin) {
        let live_route = live_isolated_route(db, &live_account, plan, spec)?;
        if !binding
            .allowed_endpoint_ids
            .iter()
            .any(|id| id == &live_route.endpoint_id)
        {
            return Err(LiveSendAuthError::unauthorized(ENDPOINT_NOT_GRANTED));
        }
        if !binding
            .allowed_origins
            .iter()
            .any(|origin| origins_match(origin, &live_route.origin))
        {
            return Err(LiveSendAuthError::unauthorized(
                "refusing to send credentials: the endpoint origin is not authorized for this Key",
            ));
        }
        ensure_secret_origin_granted(&target_url, &binding.allowed_origins)?;
        if !inference_urls_match(&target_url, &live_route.inference_url) {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
    } else {
        let endpoint_id = sealed_assigned_endpoint_id(&live_account.provider_id, spec)?;
        if !binding
            .allowed_endpoint_ids
            .iter()
            .any(|id| id == &endpoint_id)
        {
            return Err(LiveSendAuthError::unauthorized(ENDPOINT_NOT_GRANTED));
        }
        ensure_sealed_secret_origin(&target_url, &spec.base_url)?;
    }
    Ok(())
}

struct LiveIsolatedRoute {
    endpoint_id: String,
    origin: String,
    inference_url: reqwest::Url,
}

fn live_isolated_route(
    db: &crate::db::Database,
    account: &Account,
    plan: &RequestPlan,
    spec: &AttemptSpec,
) -> Result<LiveIsolatedRoute, LiveSendAuthError> {
    if let Some(destination) = db
        .custom_destination_for_account(&account.id)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
    {
        let mapping = plan
            .resolved_alias
            .as_deref()
            .and_then(|alias| {
                destination.models.iter().find(|mapping| {
                    crate::custom::custom_model_id_matches(&mapping.public_model, alias)
                })
            })
            .or_else(|| {
                plan.original_model.as_deref().and_then(|model| {
                    destination.models.iter().find(|mapping| {
                        crate::custom::custom_model_id_matches(&mapping.public_model, model)
                    })
                })
            })
            .or_else(|| {
                let mut matches = destination
                    .models
                    .iter()
                    .filter(|mapping| mapping.upstream_model.trim() == plan.model.trim());
                let first = matches.next()?;
                matches.next().is_none().then_some(first)
            })
            .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
        let route = crate::dynamic::effective_mapping_route(
            destination.protocol,
            &destination.endpoint_url,
            mapping,
        );
        let auth_kind = match destination.auth_scheme {
            ocg_domain::destination::AuthScheme::Bearer => {
                ocg_domain::dynamic::DynamicAuthKind::Bearer
            }
            ocg_domain::destination::AuthScheme::XApiKey => {
                ocg_domain::dynamic::DynamicAuthKind::XApiKey
            }
            ocg_domain::destination::AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
        };
        if spec.upstream != protocol_to_api(route.protocol)
            || spec.wire_auth() != wire_auth_for_dynamic(auth_kind)
        {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let current = resolve_inference_url(&route.endpoint_url, route.protocol)?;
        if let Some(planned) = plan.custom_route.as_ref() {
            let planned_url = resolve_inference_url(&planned.endpoint_url, route.protocol)?;
            if planned_url != current || planned.auth_kind != auth_kind {
                return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
            }
        }
        let spec_url = spec
            .request_url()
            .map_err(LiveSendAuthError::unauthorized)?;
        if !inference_urls_match(&spec_url, &current) {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let connection_id =
            connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &destination.legacy_id);
        let mut routes = vec![RouteSpec {
            operation: EndpointOperation::from(destination.protocol),
            url: Some(destination.endpoint_url.clone()),
        }];
        let mut seen_routes = std::collections::HashSet::from([(
            destination.protocol,
            destination.endpoint_url.clone(),
        )]);
        for candidate in &destination.models {
            let Some(candidate) = candidate.upstream_override.as_ref() else {
                continue;
            };
            if seen_routes.insert((candidate.protocol, candidate.endpoint_url.clone())) {
                routes.push(RouteSpec {
                    operation: EndpointOperation::from(candidate.protocol),
                    url: Some(candidate.endpoint_url.clone()),
                });
            }
        }
        let selected_operation = EndpointOperation::from(route.protocol);
        let endpoint = assigned_endpoints_for_routes(&connection_id, &routes)
            .into_iter()
            .zip(routes)
            .find(|(_, candidate)| {
                candidate.operation == selected_operation
                    && candidate.url.as_deref() == Some(route.endpoint_url.as_str())
            })
            .map(|(assigned, _)| assigned)
            .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
        let origin = normalize_origin(&route.endpoint_url)
            .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
        return Ok(LiveIsolatedRoute {
            endpoint_id: endpoint.id,
            origin,
            inference_url: current,
        });
    }

    if let Some(config) = db
        .account_custom_config(&account.id)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
    {
        let auth_kind = db
            .custom_auth_kind(&account.id)
            .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
            .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
        if spec.upstream != protocol_to_api(config.upstream_protocol)
            || spec.wire_auth() != wire_auth_for_dynamic(auth_kind)
        {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let current = resolve_inference_url(&config.endpoint_url, config.upstream_protocol)?;
        if let Some(planned) = plan.custom_route.as_ref() {
            let planned_url =
                resolve_inference_url(&planned.endpoint_url, config.upstream_protocol)?;
            if planned_url != current || planned.auth_kind != auth_kind {
                return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
            }
        }
        let spec_url = spec
            .request_url()
            .map_err(LiveSendAuthError::unauthorized)?;
        if !inference_urls_match(&spec_url, &current) {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let operation = EndpointOperation::from(config.upstream_protocol);
        let assigned = assigned_endpoints_for_routes(
            &connection_id_for_legacy(LegacyConnectionKind::CustomAccount, &account.id),
            &[RouteSpec {
                operation,
                url: Some(config.endpoint_url.clone()),
            }],
        );
        let endpoint = assigned
            .into_iter()
            .next()
            .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
        let origin = normalize_origin(&config.endpoint_url)
            .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
        return Ok(LiveIsolatedRoute {
            endpoint_id: endpoint.id,
            origin,
            inference_url: current,
        });
    }

    if db
        .provider_is_onboarding_draft(&account.provider_id)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
        == Some(true)
    {
        return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
    }
    let runtimes = db
        .list_dynamic_providers()
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?;
    let runtime = find_runtime(&runtimes, &account.provider_id)
        .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
    let mapping = runtime
        .mapping_for_upstream(&plan.model)
        .or_else(|| runtime.mapping_for_public(&plan.model))
        .or_else(|| {
            plan.resolved_alias
                .as_deref()
                .and_then(|alias| runtime.mapping_for_public(alias))
        })
        .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
    let live_route = runtime.effective_route(mapping);
    if spec.upstream != protocol_to_api(live_route.protocol)
        || spec.wire_auth() != wire_auth_for_dynamic(runtime.auth_kind)
    {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let current = resolve_inference_url(&live_route.endpoint_url, live_route.protocol)?;
    let spec_url = spec
        .request_url()
        .map_err(LiveSendAuthError::unauthorized)?;
    if !inference_urls_match(&spec_url, &current) {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let endpoint = assigned_dynamic_endpoint(runtime, &live_route)
        .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
    let origin = normalize_origin(&live_route.endpoint_url)
        .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
    Ok(LiveIsolatedRoute {
        endpoint_id: endpoint.id,
        origin,
        inference_url: current,
    })
}

fn wire_auth_for_dynamic(auth_kind: ocg_domain::dynamic::DynamicAuthKind) -> UpstreamAuth {
    match auth_kind {
        ocg_domain::dynamic::DynamicAuthKind::Bearer => UpstreamAuth::Bearer,
        ocg_domain::dynamic::DynamicAuthKind::XApiKey => UpstreamAuth::XApiKey,
        ocg_domain::dynamic::DynamicAuthKind::None => UpstreamAuth::None,
    }
}

fn assigned_dynamic_endpoint(
    runtime: &crate::dynamic::DynamicProviderRuntime,
    live_route: &crate::dynamic::DynamicEffectiveRoute,
) -> Option<AssignedEndpoint> {
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id);
    let mut routes = vec![RouteSpec {
        operation: EndpointOperation::from(runtime.upstream_protocol),
        url: Some(runtime.endpoint_url.clone()),
    }];
    let mut seen = HashSet::from([(runtime.upstream_protocol, runtime.endpoint_url.clone())]);
    for mapping in &runtime.mappings {
        let Some(override_route) = &mapping.upstream_override else {
            continue;
        };
        if !seen.insert((override_route.protocol, override_route.endpoint_url.clone())) {
            continue;
        }
        routes.push(RouteSpec {
            operation: EndpointOperation::from(override_route.protocol),
            url: Some(override_route.endpoint_url.clone()),
        });
    }
    assigned_endpoints_for_routes(&connection_id, &routes)
        .into_iter()
        .zip(routes)
        .find(|(assigned, route)| {
            route.operation == EndpointOperation::from(live_route.protocol)
                && assigned.url.as_deref() == Some(live_route.endpoint_url.as_str())
        })
        .map(|(assigned, _)| assigned)
}

fn sealed_assigned_endpoint_id(
    provider_id: &str,
    spec: &AttemptSpec,
) -> Result<String, LiveSendAuthError> {
    let operation = endpoint_operation_for_upstream(spec.upstream)?;
    let connection_id =
        connection_id_for_legacy(LegacyConnectionKind::BuiltinProvider, provider_id);
    Ok(endpoint_id_for(&connection_id, operation).to_string())
}

fn endpoint_operation_for_upstream(
    upstream: crate::kernel::protocol::ApiFormat,
) -> Result<EndpointOperation, LiveSendAuthError> {
    let protocol = match upstream {
        crate::kernel::protocol::ApiFormat::ChatCompletions => {
            UpstreamProtocolKind::ChatCompletions
        }
        crate::kernel::protocol::ApiFormat::Responses => UpstreamProtocolKind::Responses,
        crate::kernel::protocol::ApiFormat::Messages => UpstreamProtocolKind::Messages,
        crate::kernel::protocol::ApiFormat::Gemini => {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
    };
    Ok(EndpointOperation::from(protocol))
}

fn resolve_inference_url(
    endpoint_url: &str,
    protocol: UpstreamProtocolKind,
) -> Result<reqwest::Url, LiveSendAuthError> {
    resolve_custom_endpoints(endpoint_url, protocol)
        .map(|resolved| resolved.inference)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))
}

fn inference_urls_match(spec_url: &str, current: &reqwest::Url) -> bool {
    reqwest::Url::parse(spec_url.trim()).is_ok_and(|parsed| parsed == *current)
}
