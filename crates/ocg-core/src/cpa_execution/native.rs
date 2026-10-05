//! Owned native binding: sealed endpoints, route authority, and reconciliation.
//!
//! Token files are not read. Same-epoch refresh metadata advances only when the
//! stored record already carries the capability verified by `accept_ready`.

use super::ExecutionError;
use super::store::{AuthStamp, OAuthPresence, OAuthStamp, Record};
use crate::cpa::CpaOAuthProvider;
use crate::cpa_projection::{
    EffectiveNativeWire, NativeAuthorityFacts, NativeDispatchTarget, credential_route_fingerprint,
    default_grant_ids, facts_from_effective, native_route_targets,
};
use crate::db::native_binding::{self, NativeModelInsert};
use crate::provider::CPA_PROVIDER_ID;
use ocg_domain::connection::{ConnectionId, LegacyConnectionKind, connection_id_for_legacy};
use ocg_domain::credential::{ModelScope, credential_id_for_legacy_account, origins_equivalent};
use rusqlite::Transaction;
use serde_json::{Map, Value};

const CALLABLE_PROTOCOLS: [&str; 3] = ["chat_completions", "responses", "messages"];
const REFRESH_CAPABILITY: &str = "native-refresh-registration-fence-v1";

pub(crate) enum RoutePlane {
    Desired,
    Applied,
}

pub(crate) enum AdmissionUse {
    Client,
    Validated,
}

pub(crate) struct SelectedRouteQuery {
    pub credential_id: String,
    pub credential_version: u64,
    pub public_model: String,
    pub protocol: String,
    pub admission: AdmissionUse,
}

#[derive(Debug)]
pub(crate) struct SelectedRouteAuthority {
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub material_revision: String,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub validation_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredNativeRef {
    pub native_provider: String,
    pub relative_path: String,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub material_revision: String,
    pub registration_epoch: u64,
    pub models: Vec<String>,
    pub bound: bool,
    /// Redacted child fact. Missing or non-bool is not execution authority.
    pub disabled: Option<bool>,
    /// Redacted child fact. Only `active` with `disabled: false` is authority.
    pub status: Option<String>,
    /// Authenticated `rawProviderLabel`. A display name or file name is not copied here.
    pub raw_provider_label: String,
    /// Stored mode: `""`, `com`, `ai`, `cli`, or `api`. Source `claude` and `daily` store `""`.
    pub native_mode: String,
    /// Resolved generation base. It is not a dispatch target and does not grant by itself.
    pub reported_base: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiscoverySnapshot {
    Malformed,
    Incomplete,
    Complete(Vec<DiscoveredNativeRef>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StampLease {
    pub credential_id: String,
    pub credential_version: u64,
    pub auth_state_version: u64,
    pub presence: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeLease {
    pub child_generation: u64,
    pub settings_revision: u64,
    pub stamps: Vec<StampLease>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcileReport {
    pub changed: bool,
    pub needs_apply: bool,
    pub fenced: Vec<String>,
    pub epoch_refused: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FencedTarget {
    pub credential_id: String,
    pub credential_version: u64,
    pub auth_state_version: u64,
}

pub(super) struct SealedRoute {
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub native_targets: Vec<NativeDispatchTarget>,
}

/// Admit hook used by `identity.rs` before it reads current material.
///
/// Ordinary and duplicate attempts write nothing. A changed registration epoch
/// marks the one matching native stamp pending and does not copy material.
/// Same-epoch material advances only when this record already holds the
/// verified refresh capability and the product fences still match.
/// `PolicyFault::Unavailable` is reserved for a failed save: the policy service
/// turns that fault into a plane-wide unavailable decision.
pub(super) fn advance_refresh_on_tx(
    tx: &Transaction<'_>,
    record: &mut Record,
    attempt: &super::identity::CapturedAttempt,
) -> Result<(), crate::cpa_policy::PolicyFault> {
    if attempt.credential_id.is_empty() || attempt.provider_id != CPA_PROVIDER_ID {
        return Ok(());
    }
    let matches = record
        .oauth
        .iter()
        .enumerate()
        .filter(|(_, stamp)| stamp.credential_id == attempt.credential_id)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Ok(());
    }
    let index = matches[0];
    let stamp = &record.oauth[index];
    if stamp.provider_id != CPA_PROVIDER_ID
        || stamp.native_provider.is_empty()
        || stamp.auth_id != attempt.auth_id
    {
        return Ok(());
    }
    if stamp.registration_epoch != attempt.registration_epoch {
        if stamp.presence == OAuthPresence::Present {
            record.oauth[index].presence = OAuthPresence::Pending;
            super::store::save(tx, record)
                .map_err(|_| crate::cpa_policy::PolicyFault::Unavailable)?;
        }
        return Ok(());
    }
    if stamp.material_revision == attempt.material_revision {
        return Ok(());
    }
    if !record
        .host_capabilities
        .iter()
        .any(|item| item == REFRESH_CAPABILITY)
    {
        return Ok(());
    }
    if record.child_generation == 0 || record.child_generation != record.applied_generation {
        return Ok(());
    }
    if stamp.presence != OAuthPresence::Present
        || stamp.credential_version != attempt.credential_version
    {
        return Ok(());
    }
    let row = native_binding::load_owned_credential(tx, &attempt.credential_id)
        .map_err(|_| crate::cpa_policy::PolicyFault::Unavailable)?;
    let Some(row) = row else {
        return Ok(());
    };
    if row.credential_version != attempt.credential_version
        || row.provider_id != CPA_PROVIDER_ID
        || row.credential_kind != "none"
        || !row.key_empty
        || !row.enabled
        || !row.binding_enabled
        || !row.destination_enabled
        || row.binding_id.is_empty()
        || row.allowed_endpoint_ids.is_empty()
        || row.allowed_origins.is_empty()
        || !scope_allows(&row.scope_json, &attempt.public_model)
    {
        return Ok(());
    }
    let material = attempt.material_revision.clone();
    record.oauth[index].material_revision = material.clone();
    for auth in record
        .desired_auth
        .iter_mut()
        .chain(record.applied_auth.iter_mut())
    {
        if auth_matches(auth, attempt) {
            auth.material_revision = material.clone();
        }
    }
    for set in record
        .desired_routes
        .iter_mut()
        .chain(record.applied_routes.iter_mut())
    {
        if set.credential_id == attempt.credential_id
            && set.credential_version == attempt.credential_version
            && set.auth_id == attempt.auth_id
            && set.binding_id == row.binding_id
        {
            set.material_fingerprint = material.clone();
            set.fingerprint = credential_route_fingerprint(
                &set.auth_id,
                &set.credential_id,
                set.credential_version,
                &set.binding_id,
                &set.material_fingerprint,
                set.routing_rank,
                &set.routes,
            );
        }
    }
    super::store::save(tx, record).map_err(|_| crate::cpa_policy::PolicyFault::Unavailable)?;
    Ok(())
}

fn auth_matches(auth: &AuthStamp, attempt: &super::identity::CapturedAttempt) -> bool {
    auth.credential_id == attempt.credential_id
        && auth.credential_version == attempt.credential_version
        && auth.auth_id == attempt.auth_id
        && auth.registration_epoch == attempt.registration_epoch
}

pub(super) fn sealed_routes(provider: CpaOAuthProvider, model: &str) -> Vec<SealedRoute> {
    routes_from_facts(&facts_for_provider(provider), model)
}

pub(super) fn routes_from_facts(facts: &NativeAuthorityFacts, model: &str) -> Vec<SealedRoute> {
    if !facts.raw_label.trim().is_empty() {
        let normalized = crate::cpa_projection::normalize_native_label(&facts.raw_label);
        if !normalized.provider.is_empty() && normalized.provider != facts.provider {
            return Vec::new();
        }
    }
    let connection = owned_connection_id();
    CALLABLE_PROTOCOLS
        .into_iter()
        .filter_map(|protocol| {
            let (primary, targets) = native_route_targets(facts, model, protocol, &connection)?;
            if primary.endpoint_id.is_empty()
                || primary.origin.is_empty()
                || primary.endpoint_fingerprint.is_empty()
                || primary.protocol != protocol
            {
                return None;
            }
            Some(SealedRoute {
                protocol: protocol.to_string(),
                endpoint_id: primary.endpoint_id,
                origin: primary.origin,
                endpoint_fingerprint: primary.endpoint_fingerprint,
                native_targets: targets,
            })
        })
        .collect()
}

fn facts_for_provider(provider: CpaOAuthProvider) -> NativeAuthorityFacts {
    crate::cpa_projection::normalize_native_label(provider_label(provider))
}

pub(super) fn provider_label(provider: CpaOAuthProvider) -> &'static str {
    match provider {
        CpaOAuthProvider::Codex => "codex",
        CpaOAuthProvider::Anthropic => "anthropic",
        CpaOAuthProvider::Antigravity => "antigravity",
        CpaOAuthProvider::Kimi => "kimi",
        CpaOAuthProvider::Xai => "xai",
    }
}

pub(super) fn sealed_route(provider: CpaOAuthProvider, model: &str) -> Option<SealedRoute> {
    sealed_routes(provider, model).into_iter().next()
}

pub(crate) fn authorize_selected_route(
    tx: &Transaction<'_>,
    record: &Record,
    plane: RoutePlane,
    query: &SelectedRouteQuery,
) -> Result<SelectedRouteAuthority, ExecutionError> {
    if query.credential_id.is_empty() || query.credential_version == 0 || query.protocol.is_empty()
    {
        return Err(unavailable("selected native route is empty"));
    }
    if matches!(query.admission, AdmissionUse::Validated) && matches!(plane, RoutePlane::Desired) {
        return Err(unavailable("validated protocol pin is not enforced"));
    }
    let sets = match plane {
        RoutePlane::Desired => &record.desired_routes,
        RoutePlane::Applied => &record.applied_routes,
    };
    if sets.is_empty() {
        return Err(unavailable("selected native route is empty"));
    }
    let stamp = record
        .oauth
        .iter()
        .find(|stamp| stamp.credential_id == query.credential_id)
        .ok_or_else(|| unavailable("selected native reference is absent"))?;
    if stamp.presence != OAuthPresence::Present {
        return Err(unavailable("selected native reference is absent"));
    }
    if stamp.provider_id != CPA_PROVIDER_ID || stamp.native_provider.is_empty() {
        return Err(unavailable("selected native provider is not owned"));
    }
    let row = native_binding::load_owned_credential(tx, &query.credential_id)
        .map_err(|_| unavailable("selected native credential could not be read"))?
        .ok_or_else(|| unavailable("selected native credential is absent"))?;
    let account_id = native_binding::account_id_for(&stamp.native_provider, &stamp.relative_path)
        .map_err(|_| unavailable("selected native reference is not a single path"))?;
    let expected_credential = credential_id_for_legacy_account(&account_id).to_string();
    if row.account_id != account_id
        || row.credential_id != expected_credential
        || row.destination_id != native_binding::owned_destination_id()
        || row.provider_id != CPA_PROVIDER_ID
        || row.credential_kind != "none"
        || !row.key_empty
        || !row.enabled
        || !row.destination_enabled
        || row.credential_version != query.credential_version
        || stamp.credential_version != query.credential_version
    {
        return Err(unavailable("selected native credential is not current"));
    }
    if matches!(query.admission, AdmissionUse::Client) && !row.binding_enabled {
        return Err(unavailable("selected native binding is disabled"));
    }
    if !scope_allows(&row.scope_json, &query.public_model) {
        return Err(unavailable("selected native scope is empty"));
    }
    let set = sets
        .iter()
        .find(|set| {
            set.credential_id == query.credential_id
                && set.credential_version == query.credential_version
                && set.binding_id == row.binding_id
                && set.material_fingerprint == stamp.material_revision
        })
        .ok_or_else(|| unavailable("selected native route is not on the applied plane"))?;
    let route = set
        .routes
        .iter()
        .find(|route| route.public_model == query.public_model && route.protocol == query.protocol)
        .ok_or_else(|| unavailable("selected native route is empty"))?;
    if route.protocol.is_empty()
        || route.endpoint_id.is_empty()
        || route.origin.is_empty()
        || route.endpoint_fingerprint.is_empty()
        || route.upstream_model.is_empty()
    {
        return Err(unavailable("selected native route is empty"));
    }
    let validation_only = route.validation_only;
    match query.admission {
        AdmissionUse::Client if validation_only => {
            return Err(unavailable("selected native route is validation only"));
        }
        AdmissionUse::Validated if !validation_only => {
            return Err(unavailable("validated protocol pin is not enforced"));
        }
        _ => {}
    }
    if row.allowed_endpoint_ids.is_empty() || row.allowed_origins.is_empty() {
        return Err(unavailable("selected native grants are empty"));
    }
    if !row
        .allowed_endpoint_ids
        .iter()
        .any(|id| id == &route.endpoint_id)
    {
        return Err(unavailable("selected native endpoint is not granted"));
    }
    if !row
        .allowed_origins
        .iter()
        .any(|origin| origins_equivalent(origin, &route.origin))
    {
        return Err(unavailable("selected native origin is not granted"));
    }
    Ok(SelectedRouteAuthority {
        auth_id: set.auth_id.clone(),
        credential_id: set.credential_id.clone(),
        credential_version: set.credential_version,
        binding_id: set.binding_id.clone(),
        material_revision: set.material_fingerprint.clone(),
        public_model: route.public_model.clone(),
        upstream_model: route.upstream_model.clone(),
        protocol: route.protocol.clone(),
        endpoint_id: route.endpoint_id.clone(),
        origin: route.origin.clone(),
        endpoint_fingerprint: route.endpoint_fingerprint.clone(),
        validation_only,
    })
}

pub(crate) fn specialized_native_binding_allowed(
    conn: &rusqlite::Connection,
    credential_id: &str,
) -> Result<bool, ExecutionError> {
    let Some(row) = native_binding::load_owned_credential(conn, credential_id)
        .map_err(|_| unavailable("native binding could not be read"))?
    else {
        return Ok(false);
    };
    if row.destination_id != native_binding::owned_destination_id()
        || row.provider_id != CPA_PROVIDER_ID
        || row.credential_kind != "none"
        || !row.key_empty
        || row.credential_version == 0
    {
        return Ok(false);
    }
    let Some(record) =
        super::store::load(conn).map_err(|_| unavailable("native record is unreadable"))?
    else {
        return Ok(false);
    };
    let allowed = record.oauth.iter().any(|stamp| {
        stamp.presence == OAuthPresence::Present
            && stamp.credential_id == row.credential_id
            && stamp.credential_version == row.credential_version
            && stamp.provider_id == CPA_PROVIDER_ID
            && native_binding::account_id_for(&stamp.native_provider, &stamp.relative_path)
                .ok()
                .is_some_and(|account_id| {
                    account_id == row.account_id
                        && credential_id_for_legacy_account(&account_id).as_str()
                            == row.credential_id
                })
    });
    Ok(allowed)
}

pub(crate) fn persisted_child_generation(
    conn: &rusqlite::Connection,
) -> Result<u64, ExecutionError> {
    match super::store::load(conn) {
        Ok(Some(record)) => Ok(record.child_generation),
        Ok(None) => Ok(0),
        Err(()) => Err(unavailable("native record is unreadable")),
    }
}

pub(crate) fn capture_native_lease(
    conn: &rusqlite::Connection,
    child_generation: u64,
    settings_revision: u64,
) -> Result<NativeLease, ExecutionError> {
    let record = super::store::load(conn)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    if record.child_generation != child_generation && record.child_generation != 0 {
        return Err(ExecutionError::ApplyConflict(
            "native child generation moved".into(),
        ));
    }
    let mut stamps = Vec::new();
    for stamp in &record.oauth {
        if stamp.credential_id.is_empty() {
            stamps.push(StampLease {
                credential_id: String::new(),
                credential_version: stamp.credential_version,
                auth_state_version: 0,
                presence: presence_name(stamp.presence).to_string(),
            });
            continue;
        }
        let row = native_binding::load_owned_credential(conn, &stamp.credential_id)
            .map_err(|_| unavailable("native credential could not be read"))?;
        let (version, auth_version) = row
            .as_ref()
            .map(|item| (item.credential_version, item.auth_state_version))
            .unwrap_or((0, 0));
        stamps.push(StampLease {
            credential_id: stamp.credential_id.clone(),
            credential_version: version,
            auth_state_version: auth_version,
            presence: presence_name(stamp.presence).to_string(),
        });
    }
    Ok(NativeLease {
        child_generation,
        settings_revision,
        stamps,
    })
}

pub(crate) fn lease_current(
    conn: &rusqlite::Connection,
    lease: &NativeLease,
) -> Result<bool, ExecutionError> {
    let record = super::store::load(conn)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    if record.child_generation != lease.child_generation && lease.child_generation != 0 {
        return Ok(false);
    }
    if record.oauth.len() != lease.stamps.len() {
        return Ok(false);
    }
    for (stamp, leased) in record.oauth.iter().zip(&lease.stamps) {
        if stamp.credential_id != leased.credential_id
            || presence_name(stamp.presence) != leased.presence
        {
            return Ok(false);
        }
        if stamp.credential_id.is_empty() {
            continue;
        }
        let row = native_binding::load_owned_credential(conn, &stamp.credential_id)
            .map_err(|_| unavailable("native credential could not be read"))?;
        let Some(row) = row else {
            return Ok(false);
        };
        if row.credential_version != leased.credential_version
            || row.auth_state_version != leased.auth_state_version
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn reconcile_owned_discovery(
    conn: &rusqlite::Connection,
    snapshot: &DiscoverySnapshot,
    lease: &NativeLease,
) -> Result<ReconcileReport, ExecutionError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|_| unavailable("native reconciliation could not be saved"))?;
    if !lease_current(&tx, lease)? {
        return Err(ExecutionError::ApplyConflict(
            "native reconciliation moved".into(),
        ));
    }
    let report = match snapshot {
        DiscoverySnapshot::Malformed | DiscoverySnapshot::Incomplete => {
            Ok(ReconcileReport::default())
        }
        DiscoverySnapshot::Complete(refs) => reconcile_complete(&tx, refs),
    }?;
    tx.commit()
        .map_err(|_| unavailable("native reconciliation could not be saved"))?;
    Ok(report)
}

pub(crate) fn fence_mapped_target(
    conn: &rusqlite::Connection,
    name: &str,
    auth_index: &str,
) -> Result<FencedTarget, ExecutionError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|_| unavailable("native credential fence could not be saved"))?;
    let mut record = super::store::load(&tx)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    let index = record
        .oauth
        .iter()
        .position(|stamp| stamp_matches(stamp, name, auth_index));
    let Some(index) = index else {
        return Err(ExecutionError::Invalid(
            "owned native account is not bound".into(),
        ));
    };
    let credential_id = record.oauth[index].credential_id.clone();
    if credential_id.is_empty() {
        record.oauth[index].presence = OAuthPresence::Absent;
        super::store::save(&tx, &record)?;
        tx.commit()
            .map_err(|_| unavailable("native credential fence could not be saved"))?;
        return Err(ExecutionError::Invalid(
            "owned native account is not bound".into(),
        ));
    }
    let fenced = native_binding::bump_auth_fence(&tx, &credential_id)
        .map_err(|_| unavailable("native credential fence could not be saved"))?;
    record.oauth[index].presence = OAuthPresence::Absent;
    record.oauth[index].credential_version = fenced.credential_version;
    record.oauth[index].recovery.clear();
    super::store::save(&tx, &record)?;
    tx.commit()
        .map_err(|_| unavailable("native credential fence could not be saved"))?;
    Ok(FencedTarget {
        credential_id,
        credential_version: fenced.credential_version,
        auth_state_version: fenced.auth_state_version,
    })
}

pub(crate) fn note_external_failure(
    conn: &rusqlite::Connection,
    credential_id: &str,
    credential_version: u64,
    auth_state_version: u64,
) -> Result<(), ExecutionError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|_| unavailable("native recovery could not be saved"))?;
    let row = native_binding::load_owned_credential(&tx, credential_id)
        .map_err(|_| unavailable("native credential could not be read"))?;
    let Some(row) = row else {
        return Ok(());
    };
    if row.credential_version != credential_version || row.auth_state_version != auth_state_version
    {
        return Ok(());
    }
    let mut record = super::store::load(&tx)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    let Some(stamp) = record
        .oauth
        .iter_mut()
        .find(|stamp| stamp.credential_id == credential_id)
    else {
        return Ok(());
    };
    if stamp.presence == OAuthPresence::Present {
        return Ok(());
    }
    stamp.recovery = "external_failure".to_string();
    super::store::save(&tx, &record)?;
    tx.commit()
        .map_err(|_| unavailable("native recovery could not be saved"))?;
    Ok(())
}

pub(crate) fn discovery_from_ready_body(body: &str) -> DiscoverySnapshot {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return DiscoverySnapshot::Malformed;
    };
    discovery_from_ready_value(&value)
}

pub(super) fn discovery_from_ready_value(value: &Value) -> DiscoverySnapshot {
    let Some(refs) = value.get("authRefs") else {
        return DiscoverySnapshot::Incomplete;
    };
    let Some(items) = refs.as_array() else {
        return DiscoverySnapshot::Malformed;
    };
    let mut discovered = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            return DiscoverySnapshot::Malformed;
        };
        let relative = object
            .get("relativePath")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if relative.is_empty() {
            continue;
        }
        if native_binding::single_relative_path(relative).is_err() {
            return DiscoverySnapshot::Malformed;
        }
        let Ok(facts) = redacted_facts(object) else {
            return DiscoverySnapshot::Malformed;
        };
        let native_provider = facts.provider.clone();
        let credential_id = object
            .get("credentialId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let credential_version = object
            .get("credentialVersion")
            .map(decimal_value)
            .unwrap_or(0);
        let material_revision = object
            .get("materialRevision")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let models = object
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|model| !model.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let disabled = match object.get("disabled") {
            None => None,
            Some(Value::Bool(value)) => Some(*value),
            Some(_) => return DiscoverySnapshot::Malformed,
        };
        let status = match object.get("status") {
            None => None,
            Some(Value::String(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            Some(_) => return DiscoverySnapshot::Malformed,
        };
        let bound = !credential_id.is_empty()
            && credential_version > 0
            && !material_revision.is_empty()
            && known_provider(&native_provider);
        discovered.push(DiscoveredNativeRef {
            native_provider,
            relative_path: relative.to_string(),
            auth_id: object
                .get("authId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            credential_id,
            credential_version,
            material_revision,
            registration_epoch: object
                .get("registrationEpoch")
                .map(decimal_value)
                .unwrap_or(0),
            models,
            bound,
            disabled,
            status,
            raw_provider_label: facts.raw_label,
            native_mode: facts.mode,
            reported_base: facts.reported_base,
        });
    }
    DiscoverySnapshot::Complete(discovered)
}

pub(super) fn stamp_from_discovered(item: &DiscoveredNativeRef) -> OAuthStamp {
    OAuthStamp {
        relative_path: item.relative_path.clone(),
        auth_id: item.auth_id.clone(),
        credential_id: item.credential_id.clone(),
        credential_version: item.credential_version,
        material_revision: item.material_revision.clone(),
        provider_id: CPA_PROVIDER_ID.to_string(),
        native_provider: item.native_provider.clone(),
        registration_epoch: item.registration_epoch,
        models: item.models.clone(),
        presence: if item.bound && explicitly_active(item) {
            OAuthPresence::Present
        } else {
            OAuthPresence::Pending
        },
        recovery: String::new(),
        raw_provider_label: item.raw_provider_label.clone(),
        native_mode: item.native_mode.clone(),
        reported_base: item.reported_base.clone(),
    }
}

fn redacted_facts(object: &Map<String, Value>) -> Result<NativeAuthorityFacts, ()> {
    let raw = wire_text(object, "rawProviderLabel")?;
    let subtype = wire_text(object, "effectiveSubtype")?;
    let mode = wire_text(object, "effectiveMode")?;
    let base = wire_text(object, "effectiveGenerationBase")?;
    let auth_kind = wire_text(object, "effectiveAuthKind")?;
    Ok(facts_from_effective(&EffectiveNativeWire {
        raw_provider_label: raw.as_deref().unwrap_or(""),
        effective_subtype: subtype.as_deref(),
        effective_mode: mode.as_deref(),
        effective_generation_base: base.as_deref(),
        effective_auth_kind: auth_kind.as_deref(),
    }))
}

fn wire_text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(()),
    }
}

fn explicitly_active(item: &DiscoveredNativeRef) -> bool {
    item.disabled == Some(false) && item.status.as_deref() == Some("active")
}

fn reconcile_complete(
    conn: &rusqlite::Connection,
    refs: &[DiscoveredNativeRef],
) -> Result<ReconcileReport, ExecutionError> {
    let mut record = super::store::load(conn)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    let mut report = ReconcileReport::default();
    let known = refs
        .iter()
        .filter(|item| known_provider(&item.native_provider))
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < record.oauth.len() {
        let stamp = &record.oauth[index];
        let listed = known.iter().any(|item| {
            item.native_provider == stamp.native_provider
                && item.relative_path == stamp.relative_path
        });
        if stamp.presence == OAuthPresence::Present && !stamp.credential_id.is_empty() && !listed {
            let credential_id = stamp.credential_id.clone();
            let fenced = native_binding::bump_auth_fence(conn, &credential_id)
                .map_err(|_| unavailable("native credential fence could not be saved"))?;
            record.oauth[index].presence = OAuthPresence::Absent;
            record.oauth[index].credential_version = fenced.credential_version;
            report.fenced.push(credential_id);
            report.changed = true;
            report.needs_apply = true;
        }
        index += 1;
    }
    for item in known {
        let position = record.oauth.iter().position(|stamp| {
            stamp.native_provider == item.native_provider
                && stamp.relative_path == item.relative_path
        });
        if let Some(position) = position {
            remember_models(conn, item)?;
            let product_enabled = product_enabled(conn, &record.oauth[position].credential_id)?;
            let stamp = &mut record.oauth[position];
            if copy_redacted_facts(stamp, item) {
                report.changed = true;
                report.needs_apply = true;
            }
            if stamp.presence == OAuthPresence::Present {
                if !explicitly_active(item) || !product_enabled {
                    stamp.presence = OAuthPresence::Pending;
                    report.changed = true;
                    report.needs_apply = true;
                    continue;
                }
                if stamp.registration_epoch != item.registration_epoch {
                    let credential_id = stamp.credential_id.clone();
                    let fenced = native_binding::bump_auth_fence(conn, &credential_id)
                        .map_err(|_| unavailable("native credential fence could not be saved"))?;
                    stamp.presence = OAuthPresence::Pending;
                    stamp.credential_version = fenced.credential_version;
                    report.epoch_refused.push(credential_id);
                    report.changed = true;
                    report.needs_apply = true;
                }
                continue;
            }
            if !reactivate(conn, stamp, item)? {
                continue;
            }
            report.changed = true;
            report.needs_apply = true;
            continue;
        }
        if !item.bound {
            record.oauth.push(stamp_from_discovered(item));
            report.changed = true;
            continue;
        }
        let account_id = native_binding::account_id_for(&item.native_provider, &item.relative_path)
            .map_err(|_| unavailable("native account could not be saved"))?;
        let expected = credential_id_for_legacy_account(&account_id).to_string();
        if !item.credential_id.is_empty() && item.credential_id != expected {
            continue;
        }
        let bound =
            native_binding::bind_native_account(conn, &item.native_provider, &item.relative_path)
                .map_err(|_| unavailable("native account could not be saved"))?;
        if bound.created && !explicitly_active(item) {
            let updated = native_binding::set_owned_enabled(conn, &bound.credential_id, false)
                .map_err(|_| unavailable("native account could not be saved"))?;
            if !updated {
                return Err(unavailable("native account could not be saved"));
            }
        }
        if bound.created {
            let facts = authority_facts(item);
            if let Some((endpoint_ids, origins)) =
                default_grant_ids(&facts, &item.models, &owned_connection_id())
            {
                native_binding::write_initial_grants(
                    conn,
                    &bound.account_id,
                    &endpoint_ids,
                    &origins,
                )
                .map_err(|_| unavailable("native grants could not be saved"))?;
            }
        }
        remember_models(conn, item)?;
        let admit = explicitly_active(item) && product_enabled(conn, &bound.credential_id)?;
        let mut stamp = stamp_from_discovered(item);
        stamp.credential_id = bound.credential_id.clone();
        stamp.credential_version = bound.credential_version;
        stamp.presence = if admit {
            OAuthPresence::Present
        } else {
            OAuthPresence::Pending
        };
        record.oauth.push(stamp);
        report.changed = true;
        report.needs_apply |= admit;
    }
    if report.changed {
        super::store::save(conn, &record)?;
    }
    Ok(report)
}

fn remember_models(
    conn: &rusqlite::Connection,
    item: &DiscoveredNativeRef,
) -> Result<(), ExecutionError> {
    let destination_id = native_binding::ensure_owned_destination(conn)
        .map_err(|_| unavailable("native destination could not be saved"))?;
    let models = item
        .models
        .iter()
        .filter_map(|model| {
            let sealed = routes_from_facts(&authority_facts(item), model);
            if sealed.is_empty() {
                return None;
            }
            Some(NativeModelInsert {
                public_model: model.clone(),
                upstream_model: model.clone(),
                protocols: sealed.into_iter().map(|route| route.protocol).collect(),
            })
        })
        .collect::<Vec<_>>();
    native_binding::insert_native_models_if_new(conn, &destination_id, &models)
        .map_err(|_| unavailable("native models could not be saved"))?;
    native_binding::repair_retired_generate_content_protocols(conn, &destination_id)
        .map_err(|_| unavailable("native models could not be saved"))?;
    Ok(())
}

fn product_enabled(
    conn: &rusqlite::Connection,
    credential_id: &str,
) -> Result<bool, ExecutionError> {
    if credential_id.is_empty() {
        return Ok(false);
    }
    let row = native_binding::load_owned_credential(conn, credential_id)
        .map_err(|_| unavailable("native credential could not be read"))?;
    Ok(row.is_some_and(|row| row.enabled && row.destination_enabled))
}

/// Persist the product enabled preference for one owned native credential.
/// Discovery never calls this with `enabled = true`. Returns false when the
/// name is not a bound owned none credential. Version and rank stay put.
pub(crate) fn sync_owned_enabled(
    conn: &rusqlite::Connection,
    name: &str,
    auth_index: &str,
    enabled: bool,
) -> Result<bool, ExecutionError> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|_| unavailable("native credential could not be saved"))?;
    let mut record = super::store::load(&tx)
        .map_err(|_| unavailable("native record is unreadable"))?
        .unwrap_or_else(Record::empty);
    let Some(index) = record
        .oauth
        .iter()
        .position(|stamp| stamp_matches(stamp, name, auth_index))
    else {
        return Ok(false);
    };
    let credential_id = record.oauth[index].credential_id.clone();
    if credential_id.is_empty() {
        return Ok(false);
    }
    let updated = native_binding::set_owned_enabled(&tx, &credential_id, enabled)
        .map_err(|_| unavailable("native credential could not be saved"))?;
    if !updated {
        return Ok(false);
    }
    if record.oauth[index].presence == OAuthPresence::Present {
        record.oauth[index].presence = OAuthPresence::Pending;
        super::store::save(&tx, &record)?;
    }
    tx.commit()
        .map_err(|_| unavailable("native credential could not be saved"))?;
    Ok(true)
}

fn reactivate(
    conn: &rusqlite::Connection,
    stamp: &mut OAuthStamp,
    item: &DiscoveredNativeRef,
) -> Result<bool, ExecutionError> {
    if !item.bound || !explicitly_active(item) || stamp.credential_id.is_empty() {
        return Ok(false);
    }
    let row = native_binding::load_owned_credential(conn, &stamp.credential_id)
        .map_err(|_| unavailable("native credential could not be read"))?;
    let Some(row) = row else {
        return Ok(false);
    };
    if item.credential_version != row.credential_version
        || item.credential_id != row.credential_id
        || !row.enabled
        || !row.destination_enabled
    {
        return Ok(false);
    }
    stamp.presence = OAuthPresence::Present;
    stamp.material_revision = item.material_revision.clone();
    stamp.models = item.models.clone();
    copy_redacted_facts(stamp, item);
    stamp.registration_epoch = item.registration_epoch;
    stamp.credential_version = row.credential_version;
    stamp.recovery.clear();
    Ok(true)
}

fn stamp_matches(stamp: &OAuthStamp, name: &str, auth_index: &str) -> bool {
    let name = name.trim();
    let auth_index = auth_index.trim();
    (!name.is_empty()
        && (stamp.auth_id == name || stamp.relative_path == name || stamp.credential_id == name))
        || (!auth_index.is_empty()
            && (stamp.auth_id == auth_index
                || stamp.relative_path == auth_index
                || stamp.credential_id == auth_index))
}

fn scope_allows(scope_json: &str, public_model: &str) -> bool {
    if public_model.trim().is_empty() {
        return false;
    }
    match serde_json::from_str::<ModelScope>(scope_json) {
        Ok(ModelScope::All) => true,
        Ok(ModelScope::Only { models }) => {
            !models.is_empty() && models.iter().any(|model| model == public_model)
        }
        _ => false,
    }
}

fn authority_facts(item: &DiscoveredNativeRef) -> NativeAuthorityFacts {
    NativeAuthorityFacts {
        raw_label: item.raw_provider_label.clone(),
        provider: item.native_provider.clone(),
        mode: item.native_mode.clone(),
        reported_base: item.reported_base.clone(),
    }
}

fn copy_redacted_facts(stamp: &mut OAuthStamp, item: &DiscoveredNativeRef) -> bool {
    let changed = stamp.raw_provider_label != item.raw_provider_label
        || stamp.native_mode != item.native_mode
        || stamp.reported_base != item.reported_base;
    stamp.raw_provider_label = item.raw_provider_label.clone();
    stamp.native_mode = item.native_mode.clone();
    stamp.reported_base = item.reported_base.clone();
    changed
}

fn owned_connection_id() -> ConnectionId {
    connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        native_binding::OWNED_NATIVE_LEGACY_ID,
    )
}

pub(super) fn known_provider(label: &str) -> bool {
    provider_from_label(label).is_some()
}

pub(super) fn provider_from_label(label: &str) -> Option<CpaOAuthProvider> {
    match label.trim() {
        "codex" => Some(CpaOAuthProvider::Codex),
        "anthropic" => Some(CpaOAuthProvider::Anthropic),
        "antigravity" => Some(CpaOAuthProvider::Antigravity),
        "kimi" => Some(CpaOAuthProvider::Kimi),
        "xai" => Some(CpaOAuthProvider::Xai),
        _ => None,
    }
}

fn presence_name(presence: OAuthPresence) -> &'static str {
    match presence {
        OAuthPresence::Present => "present",
        OAuthPresence::Absent => "absent",
        OAuthPresence::Pending => "pending",
    }
}

fn decimal_value(value: &Value) -> u64 {
    match value {
        Value::String(text) => parse_decimal(text).unwrap_or(0),
        Value::Number(number) => number.as_u64().unwrap_or(0),
        _ => 0,
    }
}

fn parse_decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

fn unavailable(message: &str) -> ExecutionError {
    ExecutionError::Unavailable(message.to_string())
}

#[cfg(test)]
mod tests;
