//! Nonsecret configuration proof for one desired or applied plane.
//!
//! The caller supplies the transaction and record. This module does not decrypt,
//! admit, or reload the settings record.

use super::super::store::{AuthStamp, Record};
use super::{HttpFaceBlock, NativeFaceBlock};
use crate::cpa_policy::PolicyFault;
use crate::cpa_projection::{
    CredentialRouteSet, NativeEndpointPin, NativeSourceOperation, NativeTargetOutcome,
    NormalizedRoute, native_targets_for, targets_for_applied_route,
};
use crate::models::UpstreamChannel;
use crate::provider::ProviderAdapterKind;
use crate::routing_runtime::channel_for_adapter;
use ocg_domain::destination::{AdapterKind, AuthScheme, Destination};
use ocg_domain::ids::model_ids_match;
use rusqlite::Transaction;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum ConfigPlane {
    Desired,
    Applied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct ConfigAuthorityQuery {
    pub public_model: String,
    pub callable_protocol: String,
}

/// Zen is Free. Every other adapter, including CPA and custom HTTP, is Go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum ProductChannel {
    Go,
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum MaterialPosture {
    /// `AuthScheme::None` and the persisted cipher is empty. Not a decrypt.
    HttpNone,
    /// Bearer, API key, or x-api-key. Ciphertext was not decrypted.
    KeyedUnchecked,
    /// Present native stamp and the endpoint-pin capability.
    NativePresent,
    /// Native authority is not Present, or a no-auth cipher is not empty.
    Unproven,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum StaticPosture {
    Client,
    ValidationOnly,
    Excluded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum ConfigExclusion {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) enum NativeGrantDisposition {
    Granted { pins: Vec<NativeEndpointPin> },
    NotGranted,
    LocalOnly,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct NativeOperationFact {
    pub generation_kind: String,
    pub disposition: NativeGrantDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::cpa_execution) struct RouteAuthorityProof {
    pub plane: ConfigPlane,
    pub credential_id: String,
    pub credential_version: u64,
    pub current_version: Option<u64>,
    pub provider_id: String,
    pub binding_id: String,
    pub auth_id: String,
    pub registration_epoch: Option<u64>,
    pub routing_rank: u32,
    pub destination_id: String,
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub validation_only: bool,
    pub channel: ProductChannel,
    pub material: MaterialPosture,
    pub posture: StaticPosture,
    pub exclusions: Vec<ConfigExclusion>,
    pub credential_enabled: bool,
    pub binding_enabled: bool,
    pub destination_enabled: bool,
    pub destination_draft: bool,
    pub setup_step: String,
    pub native_provider: String,
    pub native_mode: String,
    pub capability_listed: bool,
    pub grants_cover: bool,
    pub native_operations: Vec<NativeOperationFact>,
    pub caller_pending: bool,
    pub secret_recheck_pending: bool,
    pub send_pending: bool,
}

pub(in crate::cpa_execution) fn configuration_authority_on(
    tx: &Transaction<'_>,
    record: &Record,
    plane: ConfigPlane,
    query: &ConfigAuthorityQuery,
) -> Result<Vec<RouteAuthorityProof>, PolicyFault> {
    let (map, sets) = plane_slices(record, plane);
    let mut proofs = Vec::new();
    for set in sets {
        if set.routes.is_empty() {
            proofs.push(empty_set_proof(tx, record, plane, map, sets, set)?);
            continue;
        }
        for route in &set.routes {
            proofs.push(prove_route(
                tx, record, plane, map, sets, set, route, query,
            )?);
        }
    }
    Ok(proofs)
}

fn plane_slices<'a>(
    record: &'a Record,
    plane: ConfigPlane,
) -> (&'a [AuthStamp], &'a [CredentialRouteSet]) {
    match plane {
        ConfigPlane::Desired => (&record.desired_auth, &record.desired_routes),
        ConfigPlane::Applied => (&record.applied_auth, &record.applied_routes),
    }
}

fn prove_route(
    tx: &Transaction<'_>,
    record: &Record,
    plane: ConfigPlane,
    map: &[AuthStamp],
    sets: &[CredentialRouteSet],
    set: &CredentialRouteSet,
    route: &NormalizedRoute,
    query: &ConfigAuthorityQuery,
) -> Result<RouteAuthorityProof, PolicyFault> {
    let admission = super::load_admission(tx, &set.credential_id)?;
    let destination = match admission.as_ref() {
        Some(row) if !row.destination_id.is_empty() => {
            super::load_destination(tx, &row.destination_id)?
        }
        _ => None,
    };
    let grants = match admission.as_ref() {
        Some(row) => super::read_route_grants(tx, &row.credential_id)?,
        None => super::GrantRows::Closed,
    };
    let mut exclusions = Vec::new();
    let stamp = aligned_stamp(map, sets, set, admission.as_ref(), &mut exclusions);
    push_row_facts(
        admission.as_ref(),
        destination.as_ref(),
        set,
        query,
        &mut exclusions,
    );
    if !model_ids_match(&route.public_model, &query.public_model) {
        push_exclusion(&mut exclusions, ConfigExclusion::Model);
    }
    if route.protocol.is_empty() || route.protocol != query.callable_protocol {
        push_exclusion(&mut exclusions, ConfigExclusion::Protocol);
    }
    if route.endpoint_id.is_empty()
        || route.origin.is_empty()
        || route.endpoint_fingerprint.is_empty()
        || route.upstream_model.is_empty()
    {
        push_exclusion(&mut exclusions, ConfigExclusion::Model);
    }
    if model_ids_match(&route.public_model, &query.public_model)
        && route.protocol == query.callable_protocol
    {
        let twins = set
            .routes
            .iter()
            .filter(|item| {
                model_ids_match(&item.public_model, &query.public_model)
                    && item.protocol == query.callable_protocol
            })
            .count();
        if twins != 1 {
            push_exclusion(&mut exclusions, ConfigExclusion::Protocol);
        }
    }
    let native = row_is_native(record, destination.as_ref(), &set.credential_id);
    let material = material_posture(record, destination.as_ref(), admission.as_ref(), native);
    if native && !matches!(material, MaterialPosture::NativePresent) {
        if !super::endpoint_pin_capability_listed(&record.host_capabilities) {
            push_exclusion(&mut exclusions, ConfigExclusion::Capability);
        }
        push_exclusion(&mut exclusions, ConfigExclusion::NativePresence);
    }
    if !native && matches!(material, MaterialPosture::Unproven) {
        push_exclusion(&mut exclusions, ConfigExclusion::Material);
    }
    let grants_cover = face_grants(
        tx,
        record,
        map,
        set,
        admission.as_ref(),
        destination.as_ref(),
        route,
        &mut exclusions,
    )?;
    let native_operations = if native {
        native_operation_facts(
            record,
            admission.as_ref(),
            route,
            &query.callable_protocol,
            &grants,
        )
    } else {
        Vec::new()
    };
    Ok(assemble(
        plane,
        set,
        route,
        admission.as_ref(),
        destination.as_ref(),
        stamp,
        record,
        material,
        exclusions,
        grants_cover,
        native_operations,
    ))
}

fn empty_set_proof(
    tx: &Transaction<'_>,
    record: &Record,
    plane: ConfigPlane,
    map: &[AuthStamp],
    sets: &[CredentialRouteSet],
    set: &CredentialRouteSet,
) -> Result<RouteAuthorityProof, PolicyFault> {
    let admission = super::load_admission(tx, &set.credential_id)?;
    let destination = match admission.as_ref() {
        Some(row) if !row.destination_id.is_empty() => {
            super::load_destination(tx, &row.destination_id)?
        }
        _ => None,
    };
    let mut exclusions = vec![ConfigExclusion::Unavailable];
    let stamp = aligned_stamp(map, sets, set, admission.as_ref(), &mut exclusions);
    let native = row_is_native(record, destination.as_ref(), &set.credential_id);
    let material = material_posture(record, destination.as_ref(), admission.as_ref(), native);
    let route = NormalizedRoute {
        public_model: String::new(),
        upstream_model: String::new(),
        protocol: String::new(),
        endpoint_id: String::new(),
        origin: String::new(),
        endpoint_fingerprint: String::new(),
        validation_only: false,
        native_targets: Vec::new(),
    };
    Ok(assemble(
        plane,
        set,
        &route,
        admission.as_ref(),
        destination.as_ref(),
        stamp,
        record,
        material,
        exclusions,
        false,
        Vec::new(),
    ))
}

/// Current CID, version, and binding follow `locate` before the route auth id.
///
/// The returned stamp is the single route-associated stamp, including when the
/// current triple is ambiguous. Its epoch stays on the structured proof.
fn aligned_stamp<'a>(
    map: &'a [AuthStamp],
    sets: &'a [CredentialRouteSet],
    set: &CredentialRouteSet,
    row: Option<&super::Admission>,
    exclusions: &mut Vec<ConfigExclusion>,
) -> Option<&'a AuthStamp> {
    let associated = route_stamps(map, set);
    let reported = if associated.len() == 1 {
        Some(associated[0])
    } else {
        None
    };
    let Some(row) = row else {
        if reported.is_none() {
            push_exclusion(
                exclusions,
                if set.binding_id.is_empty() {
                    ConfigExclusion::Rebound
                } else {
                    ConfigExclusion::Identity
                },
            );
        }
        return reported;
    };
    match current_binding(map, row) {
        CurrentBinding::MissingCredential
        | CurrentBinding::MissingVersion
        | CurrentBinding::Ambiguous => {
            push_exclusion(exclusions, ConfigExclusion::Identity);
        }
        CurrentBinding::Unbound => push_exclusion(exclusions, ConfigExclusion::Rebound),
        CurrentBinding::Unique(stamp) => {
            if stamp.auth_id.is_empty()
                || stamp.provider_id.is_empty()
                || stamp.provider_id != row.provider_id
                || stamp.auth_id != set.auth_id
                || stamp.binding_id != set.binding_id
                || sets
                    .iter()
                    .filter(|item| set_bound_to_stamp(item, stamp))
                    .count()
                    != 1
            {
                push_exclusion(exclusions, ConfigExclusion::Identity);
            }
        }
    }
    if let Some(stamp) = reported {
        if stamp.provider_id.is_empty() || stamp.provider_id != row.provider_id {
            push_exclusion(exclusions, ConfigExclusion::Identity);
        }
    }
    reported
}

enum CurrentBinding<'a> {
    MissingCredential,
    MissingVersion,
    Unbound,
    Ambiguous,
    Unique(&'a AuthStamp),
}

/// Same nonsecret triple as `locate`: credential id, current version, binding.
fn current_binding<'a>(map: &'a [AuthStamp], row: &super::Admission) -> CurrentBinding<'a> {
    let same_id: Vec<&AuthStamp> = map
        .iter()
        .filter(|stamp| stamp.credential_id == row.credential_id)
        .collect();
    if same_id.is_empty() {
        return CurrentBinding::MissingCredential;
    }
    let versioned: Vec<&AuthStamp> = same_id
        .into_iter()
        .filter(|stamp| stamp.credential_version == row.version)
        .collect();
    if versioned.is_empty() {
        return CurrentBinding::MissingVersion;
    }
    let bound: Vec<&AuthStamp> = versioned
        .into_iter()
        .filter(|stamp| !stamp.binding_id.is_empty() && stamp.binding_id == row.binding_id)
        .collect();
    match bound.len() {
        0 => CurrentBinding::Unbound,
        1 => CurrentBinding::Unique(bound[0]),
        _ => CurrentBinding::Ambiguous,
    }
}

fn route_stamps<'a>(map: &'a [AuthStamp], set: &CredentialRouteSet) -> Vec<&'a AuthStamp> {
    map.iter()
        .filter(|stamp| {
            stamp.credential_id == set.credential_id
                && stamp.credential_version == set.credential_version
                && !stamp.binding_id.is_empty()
                && stamp.binding_id == set.binding_id
                && stamp.auth_id == set.auth_id
                && !stamp.auth_id.is_empty()
        })
        .collect()
}

fn set_bound_to_stamp(set: &CredentialRouteSet, stamp: &AuthStamp) -> bool {
    set.auth_id == stamp.auth_id
        && set.credential_id == stamp.credential_id
        && set.credential_version == stamp.credential_version
        && set.binding_id == stamp.binding_id
}

fn push_row_facts(
    row: Option<&super::Admission>,
    destination: Option<&Destination>,
    set: &CredentialRouteSet,
    query: &ConfigAuthorityQuery,
    exclusions: &mut Vec<ConfigExclusion>,
) {
    let Some(row) = row else {
        push_exclusion(exclusions, ConfigExclusion::Identity);
        return;
    };
    if row.version != set.credential_version {
        push_exclusion(exclusions, ConfigExclusion::Version);
    }
    if row.binding_id.is_empty() || row.binding_id != set.binding_id {
        push_exclusion(exclusions, ConfigExclusion::Rebound);
    }
    if !row.credential_enabled || !row.binding_enabled {
        push_exclusion(exclusions, ConfigExclusion::Disabled);
    }
    if row.pending_setup {
        push_exclusion(exclusions, ConfigExclusion::Setup);
    }
    if destination.is_none() {
        push_exclusion(exclusions, ConfigExclusion::Model);
    } else if destination.is_some_and(|item| !item.enabled) {
        push_exclusion(exclusions, ConfigExclusion::Disabled);
    }
    if row.destination_draft {
        push_exclusion(exclusions, ConfigExclusion::Draft);
    }
    if !super::scope_allows(&row.scope_json, &query.public_model) {
        push_exclusion(exclusions, ConfigExclusion::Scope);
    }
}

fn row_is_native(record: &Record, destination: Option<&Destination>, credential_id: &str) -> bool {
    destination.is_some_and(|item| item.adapter == AdapterKind::Cpa)
        || !super::oauth_rows(record, credential_id).is_empty()
}

fn material_posture(
    record: &Record,
    destination: Option<&Destination>,
    row: Option<&super::Admission>,
    native: bool,
) -> MaterialPosture {
    if native {
        if row.is_some_and(|row| super::present_native(record, row).is_some()) {
            MaterialPosture::NativePresent
        } else {
            MaterialPosture::Unproven
        }
    } else if destination.is_some_and(|item| item.auth_scheme == AuthScheme::None)
        && row.is_some_and(|row| !row.material_present)
    {
        MaterialPosture::HttpNone
    } else if destination.is_some_and(|item| {
        matches!(
            item.auth_scheme,
            AuthScheme::Bearer | AuthScheme::ApiKey | AuthScheme::XApiKey
        )
    }) {
        MaterialPosture::KeyedUnchecked
    } else {
        MaterialPosture::Unproven
    }
}

fn face_grants(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    set: &CredentialRouteSet,
    row: Option<&super::Admission>,
    destination: Option<&Destination>,
    route: &NormalizedRoute,
    exclusions: &mut Vec<ConfigExclusion>,
) -> Result<bool, PolicyFault> {
    let (Some(row), Some(destination)) = (row, destination) else {
        return Ok(false);
    };
    let Some(model) = super::catalog_row(&destination.catalog, &route.upstream_model) else {
        push_exclusion(exclusions, ConfigExclusion::Model);
        return Ok(false);
    };
    if !super::name_resolves(&row.provider_id, model, &route.public_model) {
        push_exclusion(exclusions, ConfigExclusion::Model);
    }
    let Some(protocol) = super::protocol_kind(&route.protocol) else {
        push_exclusion(exclusions, ConfigExclusion::Protocol);
        return Ok(false);
    };
    if !super::protocol_open(destination, model, protocol) {
        push_exclusion(exclusions, ConfigExclusion::Protocol);
        return Ok(false);
    }
    if destination.adapter == AdapterKind::Cpa {
        let associated = route_stamps(map, set);
        let route_stamp = associated
            .first()
            .copied()
            .filter(|_| associated.len() == 1);
        let native_map: &[AuthStamp] = match route_stamp {
            Some(stamp) => std::slice::from_ref(stamp),
            None => &[],
        };
        return Ok(
            match super::native_face_block(tx, record, native_map, row, route)? {
                None => true,
                Some(NativeFaceBlock::Presence) => {
                    push_exclusion(exclusions, ConfigExclusion::NativePresence);
                    false
                }
                Some(NativeFaceBlock::Alignment) => {
                    push_exclusion(exclusions, ConfigExclusion::Identity);
                    false
                }
                Some(NativeFaceBlock::Origin | NativeFaceBlock::Targets) => {
                    push_exclusion(exclusions, ConfigExclusion::NativeTargets);
                    false
                }
                Some(NativeFaceBlock::Mode) => {
                    push_exclusion(exclusions, ConfigExclusion::NativeMode);
                    false
                }
                Some(NativeFaceBlock::Grant) => {
                    push_exclusion(exclusions, ConfigExclusion::NotGranted);
                    false
                }
            },
        );
    }
    Ok(
        match super::http_face_block(tx, row, destination, model, protocol, route)? {
            None => true,
            Some(HttpFaceBlock::Endpoint) => {
                push_exclusion(exclusions, ConfigExclusion::Model);
                false
            }
            Some(HttpFaceBlock::Grant) => {
                push_exclusion(exclusions, ConfigExclusion::NotGranted);
                false
            }
        },
    )
}

fn native_operation_facts(
    record: &Record,
    row: Option<&super::Admission>,
    route: &NormalizedRoute,
    callable_protocol: &str,
    grants: &super::GrantRows,
) -> Vec<NativeOperationFact> {
    super::GENERATION_KINDS
        .iter()
        .map(|kind| NativeOperationFact {
            generation_kind: (*kind).to_string(),
            disposition: operation_disposition(record, row, route, callable_protocol, kind, grants),
        })
        .collect()
}

fn operation_disposition(
    record: &Record,
    row: Option<&super::Admission>,
    route: &NormalizedRoute,
    callable_protocol: &str,
    kind: &str,
    grants: &super::GrantRows,
) -> NativeGrantDisposition {
    match targets_for_applied_route(route, callable_protocol, kind) {
        Some(pins)
            if pins.is_empty() && kind == "count-tokens" && route.protocol == callable_protocol =>
        {
            local_count(record, row, route, callable_protocol)
        }
        Some(pins) if pins.is_empty() => NativeGrantDisposition::Unavailable,
        Some(pins) => match super::select_granted_pins(grants, callable_protocol, pins) {
            Ok(granted) if granted.is_empty() => NativeGrantDisposition::NotGranted,
            Ok(granted) => NativeGrantDisposition::Granted { pins: granted },
            Err(_) => NativeGrantDisposition::Unavailable,
        },
        None => NativeGrantDisposition::Unavailable,
    }
}

/// Zero stored network pins are not local-count authority.
fn local_count(
    record: &Record,
    row: Option<&super::Admission>,
    route: &NormalizedRoute,
    protocol: &str,
) -> NativeGrantDisposition {
    let Some(row) = row else {
        return NativeGrantDisposition::Unavailable;
    };
    let Some(stamp) = super::present_native(record, row) else {
        return NativeGrantDisposition::Unavailable;
    };
    match native_targets_for(
        &super::native_authority(stamp),
        &route.upstream_model,
        protocol,
        NativeSourceOperation::CountTokens,
        &super::owned_native_connection(),
    ) {
        NativeTargetOutcome::LocalOnly => NativeGrantDisposition::LocalOnly,
        NativeTargetOutcome::Network(_) | NativeTargetOutcome::Unavailable => {
            NativeGrantDisposition::Unavailable
        }
    }
}

fn assemble(
    plane: ConfigPlane,
    set: &CredentialRouteSet,
    route: &NormalizedRoute,
    row: Option<&super::Admission>,
    destination: Option<&Destination>,
    stamp: Option<&AuthStamp>,
    record: &Record,
    material: MaterialPosture,
    exclusions: Vec<ConfigExclusion>,
    grants_cover: bool,
    native_operations: Vec<NativeOperationFact>,
) -> RouteAuthorityProof {
    let provider_id = stamp
        .map(|stamp| stamp.provider_id.clone())
        .filter(|id| !id.is_empty())
        .or_else(|| row.map(|row| row.provider_id.clone()))
        .unwrap_or_default();
    let oauth = super::oauth_rows(record, &set.credential_id);
    let (native_provider, native_mode) = if oauth.len() == 1 {
        (
            oauth[0].native_provider.clone(),
            oauth[0].native_mode.clone(),
        )
    } else {
        (String::new(), String::new())
    };
    let channel = product_channel(destination, &provider_id);
    let posture = if !exclusions.is_empty() {
        StaticPosture::Excluded
    } else if route.validation_only {
        StaticPosture::ValidationOnly
    } else {
        StaticPosture::Client
    };
    RouteAuthorityProof {
        plane,
        credential_id: set.credential_id.clone(),
        credential_version: set.credential_version,
        current_version: row.map(|row| row.version),
        provider_id,
        binding_id: set.binding_id.clone(),
        auth_id: set.auth_id.clone(),
        registration_epoch: stamp.map(|stamp| stamp.registration_epoch),
        routing_rank: set.routing_rank,
        destination_id: row
            .map(|row| row.destination_id.clone())
            .unwrap_or_default(),
        public_model: route.public_model.clone(),
        upstream_model: route.upstream_model.clone(),
        protocol: route.protocol.clone(),
        endpoint_id: route.endpoint_id.clone(),
        origin: route.origin.clone(),
        endpoint_fingerprint: route.endpoint_fingerprint.clone(),
        validation_only: route.validation_only,
        channel,
        material,
        posture,
        exclusions,
        credential_enabled: row.is_some_and(|row| row.credential_enabled),
        binding_enabled: row.is_some_and(|row| row.binding_enabled),
        destination_enabled: destination.is_some_and(|item| item.enabled),
        destination_draft: row.is_some_and(|row| row.destination_draft),
        setup_step: row.map(|row| row.setup_step.clone()).unwrap_or_default(),
        native_provider,
        native_mode,
        capability_listed: super::endpoint_pin_capability_listed(&record.host_capabilities),
        grants_cover,
        native_operations,
        caller_pending: true,
        secret_recheck_pending: matches!(material, MaterialPosture::KeyedUnchecked),
        send_pending: true,
    }
}

fn product_channel(destination: Option<&Destination>, provider_id: &str) -> ProductChannel {
    let kind = destination
        .map(|item| ProviderAdapterKind::from(item.adapter))
        .or_else(|| crate::dynamic::adapter_kind_for(provider_id, &[]))
        .unwrap_or(ProviderAdapterKind::ConfigurableHttp);
    match channel_for_adapter(kind) {
        UpstreamChannel::Free => ProductChannel::Free,
        UpstreamChannel::Go => ProductChannel::Go,
    }
}

fn push_exclusion(items: &mut Vec<ConfigExclusion>, item: ConfigExclusion) {
    if !items.contains(&item) {
        items.push(item);
    }
}
