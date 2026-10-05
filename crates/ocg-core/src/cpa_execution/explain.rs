//! Read-only facts for one requested model and callable protocol.
//!
//! `cpa_execution.rs` does not declare this module yet. The device owner holds
//! that file. Primary adds `pub(crate) mod explain;` after releasing it. Parent
//! private items used here (`applied_view`, `applied_tuple_ready`,
//! `same_applied_view`, `pin_capabilities_ready`) become visible at that point.
//!
//! Lock order follows the parent module: the settings guard is held for the
//! whole read, the execution-plane mutex is taken only inside `applied_view`
//! and dropped before the database lock, and the database lock is dropped
//! before the second view. Owned-process liveness is a local host observation
//! before the read transaction and again after that lock is released. The read
//! transaction is rolled back. Nothing in this file decrypts, admits, writes,
//! or builds a hop.

use super::ExecutionError;
use super::identity::explain_resolution::{self, ResolutionClass};
use super::identity::{
    ConfigAuthorityQuery, ConfigExclusion, ConfigPlane, MaterialPosture, NativeGrantDisposition,
    ProductChannel, RouteAuthorityProof, StaticPosture, configuration_authority_on,
};
use super::store::{self, AuthStamp};
use crate::cpa_policy::{
    PolicyDocument, PolicyFault, PoolMembership, RestrictionSubject, ScopedQuotaView,
    scoped_restriction_evidence,
};
use crate::cpa_projection::{
    CpaPlacement, CredentialRouteSet, NativeEndpointPin, cpa_placement_from_origins,
};
use crate::cpa_runtime::CpaRuntimeProcessHost;
use crate::db::native_binding;
use crate::models::RoutingMode;
use crate::state::CoreState;
use chrono::{DateTime, Utc};
use ocg_domain::credential::ModelScope;
use ocg_domain::ids::model_ids_match;
use rusqlite::{OptionalExtension, Transaction};
use std::collections::HashMap;

pub(crate) const CPA_SELECTION_NOT_EVALUATED: &str = "cpa_selection_not_evaluated";
pub(crate) const QUOTA_TRIAL_NOT_EVALUATED: &str = "quota_trial_not_evaluated";
pub(crate) const CONVERSATION_BINDING_NOT_EVALUATED: &str = "not_evaluated";

/// Crate-visible copy of [`ConfigPlane`]. Dashboard code cannot name the
/// identity type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutePlane {
    Desired,
    Applied,
}

/// Go is the ordinary credential lane. Free is Zen. There is no third channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutingProductChannel {
    Go,
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterialFact {
    HttpNone,
    KeyedUnchecked,
    NativePresent,
    Unproven,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoutePosture {
    Client,
    ValidationOnly,
    Excluded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteExclusion {
    Identity,
    Rebound,
    Version,
    Setup,
    Disabled,
    Draft,
    Scope,
    Model,
    Protocol,
    Capability,
    NativePresence,
    NativeMode,
    NativeTargets,
    NotGranted,
    Material,
    Unavailable,
}

/// CPA destination base against the coherent owned listener. Other adapters stay
/// `NotApplicable`. Native presence is still the shared authority proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoricalPlacement {
    NotApplicable,
    OwnedPool,
    Remote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueryResolutionKind {
    Alias,
    PinnedRaw,
}

/// Public and upstream spellings already on the proof. Not an alias catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Spelling {
    Empty,
    Same,
    DistinctUpstream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FacadePin {
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub http_method: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GrantFact {
    Granted { pins: Vec<FacadePin> },
    NotGranted,
    LocalOnly,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OperationFact {
    pub generation_kind: String,
    pub disposition: GrantFact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RouteFact {
    pub plane: RoutePlane,
    pub credential_id: String,
    pub credential_version: u64,
    pub current_version: Option<u64>,
    pub provider_id: String,
    pub binding_id: String,
    pub auth_id: String,
    pub registration_epoch: Option<u64>,
    pub routing_rank: u32,
    pub destination_id: String,
    pub legacy_account_id: String,
    pub account_label: String,
    pub destination_label: String,
    pub public_model: String,
    pub upstream_model: String,
    pub spelling: Spelling,
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub validation_only: bool,
    pub channel: RoutingProductChannel,
    pub material: MaterialFact,
    pub posture: RoutePosture,
    pub exclusions: Vec<RouteExclusion>,
    pub credential_enabled: bool,
    pub binding_enabled: bool,
    pub destination_enabled: bool,
    pub destination_draft: bool,
    pub setup_step: String,
    pub native_provider: String,
    pub native_mode: String,
    pub capability_listed: bool,
    pub grants_cover: bool,
    pub native_operations: Vec<OperationFact>,
    pub caller_pending: bool,
    pub secret_recheck_pending: bool,
    pub send_pending: bool,
    pub quota: ScopedQuotaView,
    pub known_restriction_blocks: bool,
    pub trial_pending: bool,
    /// Applied client posture with no applicable precise restriction.
    /// False for desired, validation-only, excluded, malformed, poisoned,
    /// state-changed, and historical-remote snapshots. A stopped or exited
    /// child does not clear it. Pending caller, secret, and send flags stay
    /// set. This is not inference readiness.
    pub client_configuration_eligible: bool,
    /// Destination `AdapterKind::as_str()` from the same read. Not the channel.
    pub adapter_kind: String,
    /// Historical remote CPA base. Cleared when the two views disagree on origin.
    pub migration_required: bool,
    pub historical_placement: HistoricalPlacement,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueryMapping {
    pub destination_id: String,
    pub provider_id: String,
    pub upstream_model: String,
    /// Applied client eligibility after quota and placement.
    /// Known catalog rows without that match stay false.
    pub routeable: bool,
    /// Historical remote CPA base, including a catalog row with no projected route.
    pub migration_required: bool,
    pub adapter_kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueryResolution {
    pub known: bool,
    pub kind: Option<QueryResolutionKind>,
    pub alias: Option<String>,
    pub ambiguous: bool,
    pub mappings: Vec<QueryMapping>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaneTuple {
    pub generation: u64,
    pub revision: u64,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedProjectionFacts {
    /// Stored record child generation, desired revision, and desired digest.
    pub desired: PlaneTuple,
    pub applied: PlaneTuple,
    pub apply_status: String,
    pub desired_running: bool,
    /// Live plane child generation from the captured view. Not a ready flag.
    pub runtime_child_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeFacts {
    pub poisoned: bool,
    pub state_changed: bool,
    pub stopped: bool,
    pub origin_verified: bool,
    pub verified_ready: bool,
    pub policy_ready: bool,
    pub policy_malformed: bool,
    pub tuple_aligned: bool,
    pub pin_capabilities_ready: bool,
    pub apply_status: String,
    pub unavailable: bool,
    pub owned_running_before: bool,
    pub owned_running_after: bool,
    /// True only when both local observations saw the owned child running.
    pub owned_running: bool,
}

/// CAS scalars and display config from the start of one serialized read.
///
/// The dashboard copies these onto the existing response fields. A later
/// settings mutation leaves this copy on the token captured here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedResponseMeta {
    pub settings_revision: u64,
    pub process_generation: u64,
    pub pricing_revision: String,
    pub routing_mode: RoutingMode,
    pub conversation_sticky: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedRoutingFacts {
    pub requested_model: String,
    pub callable_protocol: String,
    pub evaluated_at: DateTime<Utc>,
    pub projection: OwnedProjectionFacts,
    pub runtime: RuntimeFacts,
    pub resolution: QueryResolution,
    pub desired: Vec<RouteFact>,
    pub applied: Vec<RouteFact>,
    /// Always `None`. Rank and list order are not an SDK next pick.
    pub first_pick: Option<String>,
    pub conversation_binding: String,
    pub uncertainties: Vec<String>,
    pub captured: CapturedResponseMeta,
}

struct ResourceFact {
    legacy_account_id: String,
    account_label: String,
    destination_label: String,
    memberships: Vec<PoolMembership>,
}

struct Snapshot {
    child_generation: u64,
    applied_generation: u64,
    desired_revision: u64,
    applied_revision: u64,
    desired_digest: String,
    applied_digest: String,
    apply_status: String,
    desired_running: bool,
    policy_ready: bool,
    unavailable: bool,
    listen_port: u16,
    owned_origin: String,
    applied_auth: Vec<AuthStamp>,
    applied_routes: Vec<CredentialRouteSet>,
    policy_malformed: bool,
    host_capabilities: Vec<String>,
    resolution: QueryResolution,
    faces: Vec<explain_resolution::DestinationFace>,
    desired: Vec<RouteFact>,
    applied: Vec<RouteFact>,
}

pub(crate) fn explain_owned_routes(
    state: &CoreState,
    public_model: &str,
    callable_protocol: &str,
    now: DateTime<Utc>,
) -> Result<OwnedRoutingFacts, ExecutionError> {
    let _settings = state.settings_update.lock();
    let captured = capture_response_meta(state);
    let running_before = observe_owned_running(state);
    let before = super::applied_view(state);
    let mut snapshot = {
        let db = state.db.lock();
        let tx = db
            .conn
            .unchecked_transaction()
            .map_err(|_| unavailable("routing explanation could not open a read"))?;
        let snapshot = read_snapshot(&tx, public_model, callable_protocol, now)?;
        drop(tx);
        snapshot
    };
    let running_after = observe_owned_running(state);
    let after = super::applied_view(state);
    apply_placement(&mut snapshot.desired, &snapshot.faces, &before, &after)?;
    apply_placement(&mut snapshot.applied, &snapshot.faces, &before, &after)?;
    let mut state_changed = !super::same_applied_view(&before, &after)
        || record_disagrees(&before, &after, &snapshot)
        || control_scalars_drifted(
            captured.settings_revision,
            state.settings_revision(),
            captured.process_generation,
            state.process_generation(),
        );
    let blocks = state_changed || before.poisoned || after.poisoned || snapshot.policy_malformed;
    if blocks {
        suppress_eligibility(&mut snapshot.desired);
        suppress_eligibility(&mut snapshot.applied);
    }
    apply_mapping_facts(
        &mut snapshot.resolution,
        &snapshot.applied,
        &snapshot.faces,
        &before,
        &after,
    )?;
    if !state_changed
        && control_scalars_drifted(
            captured.settings_revision,
            state.settings_revision(),
            captured.process_generation,
            state.process_generation(),
        )
    {
        state_changed = true;
        suppress_eligibility(&mut snapshot.desired);
        suppress_eligibility(&mut snapshot.applied);
        apply_mapping_facts(
            &mut snapshot.resolution,
            &snapshot.applied,
            &snapshot.faces,
            &before,
            &after,
        )?;
    }
    let origin_verified = origins_match(&before, &after) && !state_changed;
    Ok(OwnedRoutingFacts {
        requested_model: public_model.to_string(),
        callable_protocol: callable_protocol.to_string(),
        evaluated_at: now,
        projection: OwnedProjectionFacts {
            desired: PlaneTuple {
                generation: snapshot.child_generation,
                revision: snapshot.desired_revision,
                digest: snapshot.desired_digest.clone(),
            },
            applied: PlaneTuple {
                generation: snapshot.applied_generation,
                revision: snapshot.applied_revision,
                digest: snapshot.applied_digest.clone(),
            },
            apply_status: snapshot.apply_status.clone(),
            desired_running: snapshot.desired_running,
            runtime_child_generation: before.child_generation,
        },
        runtime: RuntimeFacts {
            poisoned: before.poisoned || after.poisoned,
            state_changed,
            stopped: snapshot.apply_status == "stopped"
                || snapshot.unavailable
                || before.apply_status == "stopped"
                || after.apply_status == "stopped"
                || before.unavailable
                || after.unavailable,
            origin_verified,
            verified_ready: !state_changed && before.verified_ready && after.verified_ready,
            policy_ready: !state_changed && before.policy_ready && after.policy_ready,
            policy_malformed: snapshot.policy_malformed,
            tuple_aligned: !state_changed
                && super::applied_tuple_ready(&before, captured.process_generation)
                && super::applied_tuple_ready(&after, captured.process_generation),
            pin_capabilities_ready: !state_changed
                && super::pin_capabilities_ready(&before.host_capabilities)
                && super::pin_capabilities_ready(&after.host_capabilities),
            apply_status: snapshot.apply_status,
            unavailable: snapshot.unavailable || before.unavailable || after.unavailable,
            owned_running_before: running_before,
            owned_running_after: running_after,
            owned_running: running_before && running_after,
        },
        resolution: snapshot.resolution,
        desired: snapshot.desired,
        applied: snapshot.applied,
        first_pick: None,
        conversation_binding: CONVERSATION_BINDING_NOT_EVALUATED.to_string(),
        uncertainties: vec![
            CPA_SELECTION_NOT_EVALUATED.to_string(),
            QUOTA_TRIAL_NOT_EVALUATED.to_string(),
        ],
        captured,
    })
}

fn capture_response_meta(state: &CoreState) -> CapturedResponseMeta {
    let config = state.config();
    CapturedResponseMeta {
        settings_revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: state.pricing_snapshot().revision.clone(),
        routing_mode: config.routing_mode,
        conversation_sticky: config.conversation_sticky,
    }
}

fn control_scalars_drifted(
    start_revision: u64,
    end_revision: u64,
    start_generation: u64,
    end_generation: u64,
) -> bool {
    start_revision != end_revision || start_generation != end_generation
}

fn read_snapshot(
    tx: &Transaction<'_>,
    public_model: &str,
    callable_protocol: &str,
    now: DateTime<Utc>,
) -> Result<Snapshot, ExecutionError> {
    let record =
        store::load_tx(tx).map_err(|_| unavailable("CPA execution record could not be read"))?;
    let loaded = load_policy(tx)?;
    let query = ConfigAuthorityQuery {
        public_model: public_model.to_string(),
        callable_protocol: callable_protocol.to_string(),
    };
    let desired_proofs = configuration_authority_on(tx, &record, ConfigPlane::Desired, &query)
        .map_err(authority_fault)?;
    let applied_proofs = configuration_authority_on(tx, &record, ConfigPlane::Applied, &query)
        .map_err(authority_fault)?;
    let resolved =
        explain_resolution::resolve_current_catalog(tx, public_model).map_err(authority_fault)?;
    let resources = resources(tx, &desired_proofs, &applied_proofs)?;
    let desired = decorate_all(
        &desired_proofs,
        &resources,
        &loaded,
        &resolved.destinations,
        now,
    )?;
    let applied = decorate_all(
        &applied_proofs,
        &resources,
        &loaded,
        &resolved.destinations,
        now,
    )?;
    Ok(Snapshot {
        child_generation: record.child_generation,
        applied_generation: record.applied_generation,
        desired_revision: record.desired_revision,
        applied_revision: record.applied_revision,
        desired_digest: record.desired_digest,
        applied_digest: record.applied_digest,
        apply_status: record.apply_status,
        desired_running: record.desired_running,
        policy_ready: record.policy_ready,
        unavailable: record.unavailable,
        listen_port: record.listen_port,
        owned_origin: record.owned_origin,
        applied_auth: record.applied_auth,
        applied_routes: record.applied_routes,
        policy_malformed: matches!(loaded, Err(PolicyFault::Malformed)),
        host_capabilities: record.host_capabilities,
        resolution: resolution_fact(&resolved),
        faces: resolved.destinations,
        desired,
        applied,
    })
}

fn load_policy(
    tx: &Transaction<'_>,
) -> Result<Result<PolicyDocument, PolicyFault>, ExecutionError> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [crate::cpa_policy::SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| unavailable("restriction document could not be read"))?;
    Ok(match value {
        None => Ok(PolicyDocument::default()),
        Some(json) => PolicyDocument::from_json(&json),
    })
}

fn resources(
    tx: &Transaction<'_>,
    desired: &[RouteAuthorityProof],
    applied: &[RouteAuthorityProof],
) -> Result<HashMap<String, ResourceFact>, ExecutionError> {
    let mut out = HashMap::new();
    for proof in desired.iter().chain(applied.iter()) {
        if out.contains_key(&proof.credential_id) {
            continue;
        }
        out.insert(
            proof.credential_id.clone(),
            load_resource(tx, &proof.credential_id, &proof.destination_id)?,
        );
    }
    Ok(out)
}

fn load_resource(
    tx: &Transaction<'_>,
    credential_id: &str,
    destination_id: &str,
) -> Result<ResourceFact, ExecutionError> {
    if credential_id.is_empty() {
        return Ok(empty_resource());
    }
    let row = tx
        .query_row(
            "SELECT COALESCE(legacy_account_id, ''), COALESCE(name, ''), COALESCE(scope_json, '')
             FROM credentials WHERE id = ?1",
            [credential_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| unavailable("account label could not be read"))?;
    let Some((legacy_account_id, account_label, scope_json)) = row else {
        return Ok(empty_resource());
    };
    let destination_label = if destination_id.is_empty() {
        String::new()
    } else {
        tx.query_row(
            "SELECT COALESCE(name, '') FROM destinations WHERE id = ?1",
            [destination_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| unavailable("account label could not be read"))?
        .unwrap_or_default()
    };
    let memberships = memberships_for(tx, credential_id, &legacy_account_id, &scope_json)?;
    Ok(ResourceFact {
        legacy_account_id,
        account_label,
        destination_label,
        memberships,
    })
}

fn empty_resource() -> ResourceFact {
    ResourceFact {
        legacy_account_id: String::new(),
        account_label: String::new(),
        destination_label: String::new(),
        memberships: Vec::new(),
    }
}

/// Same pool read as `identity::memberships` and the same `pool_version: 1`
/// mapping as `identity::scopes_for`. Those helpers are private. This is the
/// canonical `quota_pool_members` join, not a second placement classifier.
fn memberships_for(
    tx: &Transaction<'_>,
    credential_id: &str,
    legacy_account_id: &str,
    scope_json: &str,
) -> Result<Vec<PoolMembership>, ExecutionError> {
    if !table_exists(tx, "quota_pool_members")? {
        return Ok(Vec::new());
    }
    let mut statement = tx
        .prepare(
            "SELECT pool_id FROM quota_pool_members
             WHERE account_id = ?1 OR account_id = ?2
             ORDER BY pool_id",
        )
        .map_err(|_| unavailable("pool membership could not be read"))?;
    let rows = statement
        .query_map([credential_id, legacy_account_id], |row| row.get(0))
        .map_err(|_| unavailable("pool membership could not be read"))?;
    let mut pools = Vec::new();
    for row in rows {
        pools.push(row.map_err(|_| unavailable("pool membership could not be read"))?);
    }
    let Ok(scope) = serde_json::from_str::<ModelScope>(scope_json) else {
        return Ok(Vec::new());
    };
    Ok(pools
        .into_iter()
        .map(|pool_id| match &scope {
            ModelScope::All => PoolMembership {
                pool_id,
                pool_version: 1,
                public_models: Vec::new(),
                all_models: true,
            },
            ModelScope::Only { models } => PoolMembership {
                pool_id,
                pool_version: 1,
                public_models: models.clone(),
                all_models: false,
            },
        })
        .collect())
}

fn table_exists(tx: &Transaction<'_>, name: &str) -> Result<bool, ExecutionError> {
    let found: Option<String> = tx
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| unavailable("pool membership could not be read"))?;
    Ok(found.is_some())
}

fn decorate_all(
    proofs: &[RouteAuthorityProof],
    resources: &HashMap<String, ResourceFact>,
    loaded: &Result<PolicyDocument, PolicyFault>,
    faces: &[explain_resolution::DestinationFace],
    now: DateTime<Utc>,
) -> Result<Vec<RouteFact>, ExecutionError> {
    let mut out = Vec::with_capacity(proofs.len());
    for proof in proofs {
        out.push(decorate(proof, resources, loaded, faces, now)?);
    }
    Ok(out)
}

fn decorate(
    proof: &RouteAuthorityProof,
    resources: &HashMap<String, ResourceFact>,
    loaded: &Result<PolicyDocument, PolicyFault>,
    faces: &[explain_resolution::DestinationFace],
    now: DateTime<Utc>,
) -> Result<RouteFact, ExecutionError> {
    let resource = resources.get(&proof.credential_id);
    let memberships = resource
        .map(|item| item.memberships.clone())
        .unwrap_or_default();
    let subject = RestrictionSubject {
        credential_id: proof.credential_id.clone(),
        credential_version: proof.credential_version,
        provider_id: proof.provider_id.clone(),
        binding_id: proof.binding_id.clone(),
        memberships,
        public_model: proof.public_model.clone(),
    };
    let quota =
        scoped_restriction_evidence(policy_ref(loaded), &subject, now).map_err(authority_fault)?;
    let (known_restriction_blocks, trial_pending, quota_open) = quota_flags(&quota);
    let client_configuration_eligible =
        proof.plane == ConfigPlane::Applied && proof.posture == StaticPosture::Client && quota_open;
    Ok(RouteFact {
        plane: plane(proof.plane),
        credential_id: proof.credential_id.clone(),
        credential_version: proof.credential_version,
        current_version: proof.current_version,
        provider_id: proof.provider_id.clone(),
        binding_id: proof.binding_id.clone(),
        auth_id: proof.auth_id.clone(),
        registration_epoch: proof.registration_epoch,
        routing_rank: proof.routing_rank,
        destination_id: proof.destination_id.clone(),
        legacy_account_id: resource
            .map(|item| item.legacy_account_id.clone())
            .unwrap_or_default(),
        account_label: resource
            .map(|item| item.account_label.clone())
            .unwrap_or_default(),
        destination_label: resource
            .map(|item| item.destination_label.clone())
            .unwrap_or_default(),
        public_model: proof.public_model.clone(),
        upstream_model: proof.upstream_model.clone(),
        spelling: spelling(&proof.public_model, &proof.upstream_model),
        protocol: proof.protocol.clone(),
        endpoint_id: proof.endpoint_id.clone(),
        origin: proof.origin.clone(),
        endpoint_fingerprint: proof.endpoint_fingerprint.clone(),
        validation_only: proof.validation_only,
        channel: channel(proof.channel),
        material: material(proof.material),
        posture: posture(proof.posture),
        exclusions: proof.exclusions.iter().copied().map(exclusion).collect(),
        credential_enabled: proof.credential_enabled,
        binding_enabled: proof.binding_enabled,
        destination_enabled: proof.destination_enabled,
        destination_draft: proof.destination_draft,
        setup_step: proof.setup_step.clone(),
        native_provider: proof.native_provider.clone(),
        native_mode: proof.native_mode.clone(),
        capability_listed: proof.capability_listed,
        grants_cover: proof.grants_cover,
        native_operations: proof
            .native_operations
            .iter()
            .map(|fact| OperationFact {
                generation_kind: fact.generation_kind.clone(),
                disposition: grant(&fact.disposition),
            })
            .collect(),
        caller_pending: proof.caller_pending,
        secret_recheck_pending: proof.secret_recheck_pending,
        send_pending: proof.send_pending,
        quota,
        known_restriction_blocks,
        trial_pending,
        client_configuration_eligible,
        adapter_kind: faces
            .iter()
            .find(|face| face.id == proof.destination_id)
            .map(|face| face.adapter_kind.clone())
            .unwrap_or_default(),
        migration_required: false,
        historical_placement: HistoricalPlacement::NotApplicable,
    })
}

fn policy_ref(
    loaded: &Result<PolicyDocument, PolicyFault>,
) -> Result<&PolicyDocument, PolicyFault> {
    match loaded {
        Ok(document) => Ok(document),
        Err(PolicyFault::Malformed) => Err(PolicyFault::Malformed),
        Err(PolicyFault::Unavailable) => Err(PolicyFault::Unavailable),
    }
}

fn quota_flags(quota: &ScopedQuotaView) -> (bool, bool, bool) {
    match quota {
        ScopedQuotaView::Malformed => (false, false, false),
        ScopedQuotaView::Unknown => (false, false, true),
        ScopedQuotaView::Evidence(rows) => {
            let known = rows.iter().any(|row| {
                row.applicable
                    && matches!(row.reset, crate::cpa_policy::ResetEvidence::Known { .. })
            });
            let trial = rows.iter().any(|row| {
                row.applicable
                    && matches!(row.reset, crate::cpa_policy::ResetEvidence::UnknownReset)
            });
            let open = !rows.iter().any(|row| row.applicable);
            (known, trial, open)
        }
    }
}

fn suppress_eligibility(routes: &mut [RouteFact]) {
    for route in routes {
        route.client_configuration_eligible = false;
    }
}

fn record_disagrees(
    before: &super::AppliedView,
    after: &super::AppliedView,
    snapshot: &Snapshot,
) -> bool {
    before.host_capabilities != snapshot.host_capabilities
        || after.host_capabilities != snapshot.host_capabilities
        || before.record_child != snapshot.child_generation
        || before.applied_generation != snapshot.applied_generation
        || before.applied_revision != snapshot.applied_revision
        || before.applied_digest != snapshot.applied_digest
        || before.apply_status != snapshot.apply_status
        || before.policy_ready != snapshot.policy_ready
        || before.unavailable != snapshot.unavailable
        || before.listen_port != snapshot.listen_port
        || before.owned_origin != snapshot.owned_origin
        || before.applied_auth != snapshot.applied_auth
        || before.applied_routes != snapshot.applied_routes
}

fn origins_match(before: &super::AppliedView, after: &super::AppliedView) -> bool {
    match (
        super::verified_owned_origin(
            &before.owned_origin,
            before.listen_port,
            &before.public_origin,
        ),
        super::verified_owned_origin(&after.owned_origin, after.listen_port, &after.public_origin),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn plane(plane: ConfigPlane) -> RoutePlane {
    match plane {
        ConfigPlane::Desired => RoutePlane::Desired,
        ConfigPlane::Applied => RoutePlane::Applied,
    }
}

fn channel(channel: ProductChannel) -> RoutingProductChannel {
    match channel {
        ProductChannel::Go => RoutingProductChannel::Go,
        ProductChannel::Free => RoutingProductChannel::Free,
    }
}

fn material(material: MaterialPosture) -> MaterialFact {
    match material {
        MaterialPosture::HttpNone => MaterialFact::HttpNone,
        MaterialPosture::KeyedUnchecked => MaterialFact::KeyedUnchecked,
        MaterialPosture::NativePresent => MaterialFact::NativePresent,
        MaterialPosture::Unproven => MaterialFact::Unproven,
    }
}

fn posture(posture: StaticPosture) -> RoutePosture {
    match posture {
        StaticPosture::Client => RoutePosture::Client,
        StaticPosture::ValidationOnly => RoutePosture::ValidationOnly,
        StaticPosture::Excluded => RoutePosture::Excluded,
    }
}

fn exclusion(exclusion: ConfigExclusion) -> RouteExclusion {
    match exclusion {
        ConfigExclusion::Identity => RouteExclusion::Identity,
        ConfigExclusion::Rebound => RouteExclusion::Rebound,
        ConfigExclusion::Version => RouteExclusion::Version,
        ConfigExclusion::Setup => RouteExclusion::Setup,
        ConfigExclusion::Disabled => RouteExclusion::Disabled,
        ConfigExclusion::Draft => RouteExclusion::Draft,
        ConfigExclusion::Scope => RouteExclusion::Scope,
        ConfigExclusion::Model => RouteExclusion::Model,
        ConfigExclusion::Protocol => RouteExclusion::Protocol,
        ConfigExclusion::Capability => RouteExclusion::Capability,
        ConfigExclusion::NativePresence => RouteExclusion::NativePresence,
        ConfigExclusion::NativeMode => RouteExclusion::NativeMode,
        ConfigExclusion::NativeTargets => RouteExclusion::NativeTargets,
        ConfigExclusion::NotGranted => RouteExclusion::NotGranted,
        ConfigExclusion::Material => RouteExclusion::Material,
        ConfigExclusion::Unavailable => RouteExclusion::Unavailable,
    }
}

fn grant(disposition: &NativeGrantDisposition) -> GrantFact {
    match disposition {
        NativeGrantDisposition::Granted { pins } => GrantFact::Granted {
            pins: pins.iter().map(pin_fact).collect(),
        },
        NativeGrantDisposition::NotGranted => GrantFact::NotGranted,
        NativeGrantDisposition::LocalOnly => GrantFact::LocalOnly,
        NativeGrantDisposition::Unavailable => GrantFact::Unavailable,
    }
}

fn pin_fact(pin: &NativeEndpointPin) -> FacadePin {
    FacadePin {
        protocol: pin.protocol.clone(),
        endpoint_id: pin.endpoint_id.clone(),
        origin: pin.origin.clone(),
        endpoint_fingerprint: pin.endpoint_fingerprint.clone(),
        http_method: pin.http_method.clone(),
    }
}

fn spelling(public_model: &str, upstream: &str) -> Spelling {
    if public_model.is_empty() && upstream.is_empty() {
        Spelling::Empty
    } else if model_ids_match(public_model, upstream) {
        Spelling::Same
    } else {
        Spelling::DistinctUpstream
    }
}

fn observe_owned_running(state: &CoreState) -> bool {
    state
        .cpa_runtime
        .installed_host()
        .ok()
        .is_some_and(|host| host.owned_running())
}

fn resolution_fact(resolved: &explain_resolution::CatalogResolution) -> QueryResolution {
    QueryResolution {
        known: resolved.known,
        kind: resolved.kind.map(|kind| match kind {
            ResolutionClass::Alias => QueryResolutionKind::Alias,
            ResolutionClass::PinnedRaw => QueryResolutionKind::PinnedRaw,
        }),
        alias: resolved.alias.clone(),
        ambiguous: resolved.ambiguous,
        mappings: resolved
            .mappings
            .iter()
            .map(|mapping| QueryMapping {
                destination_id: mapping.destination_id.clone(),
                provider_id: mapping.provider_id.clone(),
                upstream_model: mapping.upstream_model.clone(),
                routeable: false,
                migration_required: false,
                adapter_kind: mapping.adapter_kind.clone(),
            })
            .collect(),
    }
}

fn apply_placement(
    routes: &mut [RouteFact],
    faces: &[explain_resolution::DestinationFace],
    before: &super::AppliedView,
    after: &super::AppliedView,
) -> Result<(), ExecutionError> {
    for route in routes {
        let Some(face) = faces.iter().find(|face| face.id == route.destination_id) else {
            continue;
        };
        match classify_face(face, &route.destination_id, before, after)? {
            Some(HistoricalPlacement::OwnedPool) => {
                route.historical_placement = HistoricalPlacement::OwnedPool;
                route.migration_required = false;
            }
            Some(HistoricalPlacement::Remote) => {
                route.historical_placement = HistoricalPlacement::Remote;
                route.migration_required = true;
                route.client_configuration_eligible = false;
            }
            Some(HistoricalPlacement::NotApplicable) | None => {}
        }
    }
    Ok(())
}

fn apply_mapping_facts(
    resolution: &mut QueryResolution,
    applied: &[RouteFact],
    faces: &[explain_resolution::DestinationFace],
    before: &super::AppliedView,
    after: &super::AppliedView,
) -> Result<(), ExecutionError> {
    for mapping in &mut resolution.mappings {
        let placement = match faces.iter().find(|face| face.id == mapping.destination_id) {
            Some(face) => classify_face(face, &mapping.destination_id, before, after)?,
            None => None,
        };
        let remote = placement == Some(HistoricalPlacement::Remote);
        mapping.migration_required = remote;
        mapping.routeable = !remote
            && applied.iter().any(|route| {
                route.client_configuration_eligible
                    && route.destination_id == mapping.destination_id
                    && route.provider_id == mapping.provider_id
                    && model_ids_match(&route.upstream_model, &mapping.upstream_model)
            });
    }
    Ok(())
}

fn classify_face(
    face: &explain_resolution::DestinationFace,
    destination_id: &str,
    before: &super::AppliedView,
    after: &super::AppliedView,
) -> Result<Option<HistoricalPlacement>, ExecutionError> {
    if face.adapter_kind != ocg_domain::destination::AdapterKind::Cpa.as_str() {
        return Ok(None);
    }
    if before.owned_origin != after.owned_origin {
        return Ok(None);
    }
    let owned_origin = before.owned_origin.as_str();
    let owned_origins: Vec<&str> = if owned_origin.is_empty() {
        Vec::new()
    } else {
        vec![owned_origin]
    };
    let owned_destination = native_binding::owned_destination_id();
    let owned_ids = [owned_destination.as_str()];
    match cpa_placement_from_origins(
        face.base_url.trim(),
        destination_id,
        &owned_origins,
        &owned_ids,
    ) {
        Ok(CpaPlacement::OwnedPool) => Ok(Some(HistoricalPlacement::OwnedPool)),
        Ok(CpaPlacement::Remote) => Ok(Some(HistoricalPlacement::Remote)),
        Err(error) => Err(ExecutionError::Unavailable(error.detail)),
    }
}

fn unavailable(message: &str) -> ExecutionError {
    ExecutionError::Unavailable(message.to_string())
}

fn authority_fault(fault: PolicyFault) -> ExecutionError {
    match fault {
        PolicyFault::Malformed => unavailable("routing configuration could not be read"),
        PolicyFault::Unavailable => unavailable("routing configuration is unavailable"),
    }
}

#[cfg(test)]
mod tests;
