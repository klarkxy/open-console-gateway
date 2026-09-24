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
use crate::quota_recovery::{QuotaAcquire, QuotaEpisode};
use crate::routing_snapshot::RoutingSnapshot;
use crate::state::CoreState;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};
use ocg_domain::credential::{
    AssignedEndpoint, RouteSpec, assigned_endpoints_for_routes, normalize_origin,
};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, http_model_route,
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
    pub target: Option<crate::gateway::materialize::FrozenTarget>,
    pub credential_id: Option<String>,
    pub attempt_spec: Option<AttemptSpec>,
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
            target: None,
            credential_id: None,
            attempt_spec: None,
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

impl LiveSendSelection {
    pub(crate) fn from_execution(
        route: &crate::gateway::materialize::ExecutionRoute,
        client_model: &str,
        routing_model: &str,
    ) -> Self {
        let c = &route.routing.account;
        Self {
            target: Some(route.target.clone()),
            credential_id: Some(c.credential_id.clone()),
            attempt_spec: Some(route.spec.clone()),
            account_id: c.id.clone(),
            binding_id: c.binding_id.clone(),
            credential_version: c.credential_version,
            key_cipher: c.key_cipher.clone(),
            client_model: client_model.into(),
            routing_model: routing_model.into(),
            plan_model: route.plan.model.clone(),
        }
    }
}

/// Pure authorization, shared by explain and actual dispatch. No decrypt,
/// selector mutation, database access, or route re-resolution.
pub(crate) fn verify_execution_authorization(
    snapshot: &crate::routing_snapshot::RoutingSnapshot,
    selection: &LiveSendSelection,
    spec: &AttemptSpec,
    wall: chrono::DateTime<chrono::Utc>,
    free_available: bool,
) -> Result<(), LiveSendAuthError> {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    let deny = || LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT);
    let target = selection.target.as_ref().ok_or_else(deny)?;
    if selection.attempt_spec.as_ref() != Some(spec) {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|d| d.id == target.destination.id)
        .ok_or_else(deny)?;
    let c = snapshot
        .credentials
        .iter()
        .find(|c| c.id == selection.account_id)
        .ok_or_else(deny)?;
    let channel = crate::routing_runtime::channel_for_adapter(destination.adapter.into());
    if selection.credential_id.as_deref() != Some(c.credential_id.as_str())
        || c.destination_id != target.destination.id
        || !c.enabled
        || !c.ready
        || !c.binding_enabled
        || c.auth_error.is_some()
        || c.is_cooling_for(channel, wall)
        || c.quota_probe
        || c.quota_recovery
            .as_ref()
            .is_some_and(|recovery| !recovery.due_at(wall))
        || (channel == crate::models::UpstreamChannel::Free && !free_available)
        || c.binding_id.is_empty()
        || c.binding_id != selection.binding_id
        || c.credential_version != selection.credential_version
        || c.key_cipher != selection.key_cipher
        || !binding_allows_requested_model(
            &c.scope,
            &selection.client_model,
            &selection.routing_model,
            [&selection.plan_model, &target.model.public_model],
        )
    {
        return Err(deny());
    }
    let frozen = &target.destination;
    if !destination.enabled
        || destination.adapter != frozen.adapter
        || destination.base_url != frozen.base_url
        || destination.auth_scheme != frozen.auth_scheme
        || destination.protocols != frozen.protocols
        || destination.protocol_routes != frozen.protocol_routes
        || destination.model_resolution != frozen.model_resolution
        || !destination
            .catalog
            .iter()
            .any(|m| m == &target.model && m.enabled)
        || crate::gateway::materialize::endpoint_id_for_target(
            c,
            destination,
            &target.model,
            spec.upstream,
        )
        .map_err(LiveSendAuthError::unauthorized)?
            != target.endpoint_id
    {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let auth_scheme = if destination.adapter == AdapterKind::Http {
        let protocol = match spec.upstream {
            crate::gateway::protocol::ApiFormat::ChatCompletions => {
                ocg_domain::destination::Protocol::ChatCompletions
            }
            crate::gateway::protocol::ApiFormat::Responses => {
                ocg_domain::destination::Protocol::Responses
            }
            crate::gateway::protocol::ApiFormat::Messages => {
                ocg_domain::destination::Protocol::Messages
            }
            _ => return Err(deny()),
        };
        ocg_domain::destination::http_model_route(destination, &target.model, protocol)
            .ok_or_else(deny)?
            .auth_scheme
    } else {
        destination.auth_scheme
    };
    if auth_scheme != AuthScheme::None {
        if c.key_cipher.is_empty() {
            return Err(deny());
        }
        if !matches!(&spec.credential, CredentialHandle::Account { id } if id == &c.id) {
            return Err(deny());
        }
        // CPA is a local external integration and owns its key's scope. It is
        // still subject to identity, rotation, enablement and route checks above.
        if destination.adapter != AdapterKind::Cpa
            && !c.grants.allowed_endpoint_ids.contains(&target.endpoint_id)
        {
            return Err(LiveSendAuthError::unauthorized(ENDPOINT_NOT_GRANTED));
        }
        let url = spec
            .request_url()
            .map_err(LiveSendAuthError::unauthorized)?;
        if destination.adapter == AdapterKind::Http {
            ensure_secret_origin_granted(&url, &c.grants.allowed_origins)?;
        } else {
            ensure_sealed_secret_origin(&url, &spec.base_url)?;
        }
    } else if !matches!(spec.credential, CredentialHandle::None) {
        return Err(deny());
    }
    Ok(())
}

pub(crate) fn authorize_execution_send(
    state: &CoreState,
    selection: &LiveSendSelection,
    spec: &AttemptSpec,
    decrypt: bool,
) -> Result<Option<String>, LiveSendAuthError> {
    let db = state.db.lock();
    let snapshot = load_snapshot_with_probes(state, &db)?;
    let wall = state.sample_gateway_clock().0;
    let free_available = db
        .free_channel_cooldown_until_at(wall)
        .map_err(|e| LiveSendAuthError::unauthorized(e.to_string()))?
        .is_none()
        && !crate::destination_projection::free_channel_exhausted(&snapshot.projection, wall);
    verify_execution_authorization(&snapshot, selection, spec, wall, free_available)?;
    if decrypt && matches!(spec.credential, CredentialHandle::Account { .. }) {
        state
            .decrypt_key(&selection.key_cipher)
            .map(Some)
            .map_err(LiveSendAuthError::Decrypt)
    } else {
        Ok(None)
    }
}

/// Re-check authorization immediately before send and acquire a one-shot
/// quota trial when this Key is due.
pub(crate) fn confirm_execution_send(
    state: &CoreState,
    selection: &LiveSendSelection,
    spec: &AttemptSpec,
) -> Result<Option<QuotaEpisode>, LiveSendAuthError> {
    let _settings_update = state.settings_update.lock();
    let db = state.db.lock();
    let mut snapshot = load_snapshot_with_probes(state, &db)?;
    let wall = state.sample_gateway_clock().0;
    let free_available = db
        .free_channel_cooldown_until_at(wall)
        .map_err(|e| LiveSendAuthError::unauthorized(e.to_string()))?
        .is_none()
        && !crate::destination_projection::free_channel_exhausted(&snapshot.projection, wall);
    verify_execution_authorization(&snapshot, selection, spec, wall, free_available)?;
    let Some(credential) = snapshot
        .credentials
        .iter_mut()
        .find(|credential| credential.id == selection.account_id)
    else {
        return Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT));
    };
    match acquire_quota_trial_locked(state, &db, credential, wall)? {
        QuotaAcquire::NotInRecovery => Ok(None),
        QuotaAcquire::Trial(episode) => Ok(Some(episode)),
        QuotaAcquire::SkipWaiting | QuotaAcquire::SkipProbing => {
            Err(LiveSendAuthError::unauthorized(UNAUTHORIZED_ATTEMPT))
        }
    }
}

fn load_snapshot_with_probes(
    state: &CoreState,
    db: &crate::db::Database,
) -> Result<crate::routing_snapshot::RoutingSnapshot, LiveSendAuthError> {
    let mut snapshot = crate::routing_snapshot::RoutingSnapshot::load(db)
        .map_err(|e| LiveSendAuthError::unauthorized(e.to_string()))?;
    let probes = state.quota_probes.lock();
    snapshot.apply_quota_probes(&probes);
    Ok(snapshot)
}

fn acquire_quota_trial_locked(
    state: &CoreState,
    db: &crate::db::Database,
    credential: &crate::routing_snapshot::ExecutionCredential,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<QuotaAcquire, LiveSendAuthError> {
    let Some(recovery) = credential.quota_recovery.as_ref() else {
        return Ok(QuotaAcquire::NotInRecovery);
    };
    let mut probes = state.quota_probes.lock();
    if probes
        .get(&credential.credential_id)
        .is_some_and(|episode| credential.matches_quota_episode(episode))
    {
        return Ok(QuotaAcquire::SkipProbing);
    }
    if !recovery.due_at(now) {
        return Ok(QuotaAcquire::SkipWaiting);
    }
    let episode = QuotaEpisode {
        credential_id: credential.credential_id.clone(),
        account_id: credential.id.clone(),
        credential_version: credential.credential_version,
        epoch: recovery.epoch,
        key_cipher: credential.key_cipher.clone(),
    };
    let crash_safe = recovery.with_crash_safe_retry(now);
    let saved = crate::db::quota_recovery::save_on(&db.conn, &episode, &crash_safe)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?;
    if !saved {
        return Ok(QuotaAcquire::NotInRecovery);
    }
    probes.insert(credential.credential_id.clone(), episode.clone());
    state.bump_settings_revision();
    Ok(QuotaAcquire::Trial(episode))
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

pub(super) fn verify_live_send(
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
        let public_model = selected_public_identity(selection)
            .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
        let live_route = live_isolated_route(db, &live_account, plan, spec, public_model)?;
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
    public_model: &str,
) -> Result<LiveIsolatedRoute, LiveSendAuthError> {
    if let Some(route) = live_http_destination_route(db, account, plan, spec, public_model)? {
        return Ok(route);
    }
    if let Some(destination) = db
        .custom_destination_for_account(&account.id)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?
    {
        let mapping = destination
            .models
            .iter()
            .find(|mapping| {
                crate::custom::custom_model_id_matches(&mapping.public_model, public_model)
            })
            .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
        if mapping.upstream_model.trim() != plan.model.trim() {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let route = crate::dynamic::effective_mapping_route(
            destination.protocol,
            &destination.endpoint_url,
            mapping,
        );
        let auth_kind = auth_kind_from_scheme(destination.auth_scheme);
        if spec.upstream != protocol_to_api(route.protocol)
            || spec.wire_auth() != wire_auth_for_dynamic(auth_kind)
        {
            return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
        }
        let current = resolve_inference_url(&route.endpoint_url, route.protocol)?;
        confirm_planned_custom_route(plan, &current, route.protocol, auth_kind)?;
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
        confirm_planned_custom_route(plan, &current, config.upstream_protocol, auth_kind)?;
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
        .mapping_for_public(public_model)
        .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
    if mapping.upstream_model.trim() != plan.model.trim() {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
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

fn live_http_destination_route(
    db: &crate::db::Database,
    account: &Account,
    plan: &RequestPlan,
    spec: &AttemptSpec,
    public_model: &str,
) -> Result<Option<LiveIsolatedRoute>, LiveSendAuthError> {
    let snapshot = RoutingSnapshot::load(db)
        .map_err(|error| LiveSendAuthError::unauthorized(error.to_string()))?;
    let Some(credential) = snapshot
        .credentials
        .iter()
        .find(|credential| credential.id == account.id)
    else {
        return Ok(None);
    };
    let Some(destination) = snapshot
        .projection
        .destinations
        .iter()
        .find(|destination| destination.id == credential.destination_id)
    else {
        return Ok(None);
    };
    if destination.adapter != AdapterKind::Http || destination.capabilities.observer {
        return Ok(None);
    }
    let mapping = catalog_model_for_public(destination, public_model)
        .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
    if mapping.upstream_model.trim() != plan.model.trim() {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let protocol = protocol_from_upstream(spec.upstream)?;
    let selected = http_model_route(destination, mapping, protocol)
        .ok_or_else(|| LiveSendAuthError::unauthorized(ROUTE_CHANGED))?;
    let auth_kind = auth_kind_from_scheme(selected.auth_scheme);
    if spec.upstream != protocol_to_api(selected.protocol)
        || spec.wire_auth() != wire_auth_for_dynamic(auth_kind)
    {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let current = resolve_inference_url(&selected.endpoint_url, selected.protocol)?;
    confirm_planned_custom_route(plan, &current, selected.protocol, auth_kind)?;
    let spec_url = spec
        .request_url()
        .map_err(LiveSendAuthError::unauthorized)?;
    if !inference_urls_match(&spec_url, &current) {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    let endpoint_id = crate::gateway::materialize::endpoint_id_for_target(
        credential,
        destination,
        mapping,
        spec.upstream,
    )
    .map_err(LiveSendAuthError::unauthorized)?;
    let origin = normalize_origin(&selected.endpoint_url)
        .ok_or_else(|| LiveSendAuthError::unauthorized(MISSING_GRANT))?;
    Ok(Some(LiveIsolatedRoute {
        endpoint_id,
        origin,
        inference_url: current,
    }))
}

fn selected_public_identity(selection: &LiveSendSelection) -> Option<&str> {
    if let Some(target) = selection.target.as_ref() {
        let name = target.model.public_model.as_str();
        if !name.trim().is_empty() {
            return Some(name);
        }
    }
    let name = selection.routing_model.as_str();
    if name.trim().is_empty() {
        None
    } else {
        Some(name)
    }
}

fn catalog_model_for_public<'a>(
    destination: &'a Destination,
    public_model: &str,
) -> Option<&'a CatalogModel> {
    destination
        .catalog
        .iter()
        .find(|model| crate::custom::custom_model_id_matches(&model.public_model, public_model))
}

fn confirm_planned_custom_route(
    plan: &RequestPlan,
    current: &reqwest::Url,
    protocol: UpstreamProtocolKind,
    auth_kind: ocg_domain::dynamic::DynamicAuthKind,
) -> Result<(), LiveSendAuthError> {
    let Some(planned) = plan.custom_route.as_ref() else {
        return Ok(());
    };
    let planned_url = resolve_inference_url(&planned.endpoint_url, protocol)?;
    if planned_url != *current || planned.auth_kind != auth_kind {
        return Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED));
    }
    Ok(())
}

fn protocol_from_upstream(
    upstream: crate::kernel::protocol::ApiFormat,
) -> Result<UpstreamProtocolKind, LiveSendAuthError> {
    match upstream {
        crate::kernel::protocol::ApiFormat::ChatCompletions => {
            Ok(UpstreamProtocolKind::ChatCompletions)
        }
        crate::kernel::protocol::ApiFormat::Responses => Ok(UpstreamProtocolKind::Responses),
        crate::kernel::protocol::ApiFormat::Messages => Ok(UpstreamProtocolKind::Messages),
        crate::kernel::protocol::ApiFormat::Gemini => {
            Err(LiveSendAuthError::unauthorized(ROUTE_CHANGED))
        }
    }
}

fn auth_kind_from_scheme(scheme: AuthScheme) -> ocg_domain::dynamic::DynamicAuthKind {
    match scheme {
        AuthScheme::Bearer => ocg_domain::dynamic::DynamicAuthKind::Bearer,
        AuthScheme::XApiKey => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
        AuthScheme::ApiKey => ocg_domain::dynamic::DynamicAuthKind::ApiKey,
        AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
    }
}

fn wire_auth_for_dynamic(auth_kind: ocg_domain::dynamic::DynamicAuthKind) -> UpstreamAuth {
    match auth_kind {
        ocg_domain::dynamic::DynamicAuthKind::Bearer => UpstreamAuth::Bearer,
        ocg_domain::dynamic::DynamicAuthKind::XApiKey => UpstreamAuth::XApiKey,
        ocg_domain::dynamic::DynamicAuthKind::ApiKey => UpstreamAuth::ApiKey,
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

/// Late observations may update only the exact identity that performed I/O.
/// The caller retains the DB lock through the subsequent state write.
pub(crate) fn selection_identity_is_current(
    db: &crate::db::Database,
    selected: &LiveSendSelection,
) -> anyhow::Result<bool> {
    use rusqlite::OptionalExtension;
    let live = db.conn.query_row(
        "SELECT id, binding_id, credential_version, key_cipher FROM credentials WHERE legacy_account_id = ?1 AND COALESCE(credential_purpose, 'inference') = 'inference'",
        [&selected.account_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?))).optional()?;
    Ok(live.is_some_and(|(id, binding, version, cipher)| {
        selected.credential_id.as_deref() == Some(id.as_str())
            && binding == selected.binding_id
            && version > 0
            && version as u64 == selected.credential_version
            && cipher == selected.key_cipher
    }))
}

/// Revalidate the route that produced an observation, without treating its own
/// quota trial or a concurrent cooldown as an authorization change.
/// The caller holds the DB lock through its state write.
pub(crate) fn selection_allows_observation(
    db: &crate::db::Database,
    selected: &LiveSendSelection,
) -> anyhow::Result<bool> {
    let Some(spec) = selected.attempt_spec.as_ref() else {
        return Ok(false);
    };
    let mut snapshot = crate::routing_snapshot::RoutingSnapshot::load(db)?;
    if let Some(credential) = snapshot
        .credentials
        .iter_mut()
        .find(|credential| credential.id == selected.account_id)
    {
        credential.quota_recovery = None;
        credential.quota_probe = false;
        credential.cooldowns = ocg_domain::destination::Cooldowns {
            generic_until: None,
            five_hour_until: None,
            week_until: None,
            month_until: None,
            free_until: None,
        };
    }
    Ok(verify_execution_authorization(&snapshot, selected, spec, chrono::Utc::now(), true).is_ok())
}

#[cfg(test)]
mod tests;
