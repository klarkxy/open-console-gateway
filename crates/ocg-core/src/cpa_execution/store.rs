//! Desired and applied CPA projection record in the existing settings table.
//!
//! Secrets are not stored here. Invalid JSON poisons the plane.

use super::ExecutionError;
use super::io::MAX_CONFIG_BYTES;
use crate::cpa_projection::{
    CredentialRouteSet, NativeDispatchTarget, NormalizedRoute, accept_stored_native_targets,
    native_mode_ok,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OAuthPresence {
    Present,
    Absent,
    Pending,
}

pub(super) const SETTINGS_KEY: &str = "cpa_execution_projection_v1";
const RECORD_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AuthStamp {
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub material_revision: String,
    pub provider_id: String,
    pub registration_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OAuthStamp {
    pub relative_path: String,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub material_revision: String,
    pub provider_id: String,
    pub native_provider: String,
    pub registration_epoch: u64,
    pub models: Vec<String>,
    pub presence: OAuthPresence,
    pub recovery: String,
    /// Trimmed discovery label. Empty on historical records.
    pub raw_provider_label: String,
    /// `""`, `com`, `ai`, `cli`, or `api`.
    pub native_mode: String,
    /// Redacted base fact. Empty on historical records.
    pub reported_base: String,
}

/// One accepted projection saved for the single `config.yaml.previous` slot.
///
/// `None` is an unknown historical file. An empty auth and route list is a
/// real accepted empty projection, not that unknown state. The snapshot does
/// not carry tokens, readiness, capabilities, or OAuth presence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AcceptedProjectionSnapshot {
    pub generation: u64,
    pub revision: u64,
    pub wire_digest: String,
    pub auth: Vec<AuthStamp>,
    pub routes: Vec<CredentialRouteSet>,
    pub artifact_sha256: String,
    pub listen_port: u16,
    pub owned_origin: String,
    pub public_origin: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Record {
    pub child_generation: u64,
    pub applied_generation: u64,
    pub desired_revision: u64,
    pub applied_revision: u64,
    pub desired_digest: String,
    pub applied_digest: String,
    pub artifact_sha256: String,
    pub listen_port: u16,
    pub apply_status: String,
    pub desired_running: bool,
    pub public_origin: String,
    pub owned_origin: String,
    pub policy_ready: bool,
    pub unavailable: bool,
    pub desired_auth: Vec<AuthStamp>,
    pub applied_auth: Vec<AuthStamp>,
    pub desired_routes: Vec<CredentialRouteSet>,
    pub applied_routes: Vec<CredentialRouteSet>,
    /// Verified required capabilities from an accepted ready document.
    /// Missing historical JSON defaults empty and grants nothing.
    pub host_capabilities: Vec<String>,
    pub oauth: Vec<OAuthStamp>,
    /// Typed authority of the one previous YAML file. Missing JSON is unknown.
    pub previous_accepted: Option<AcceptedProjectionSnapshot>,
}

impl Record {
    pub(super) fn empty() -> Self {
        Self {
            child_generation: 0,
            applied_generation: 0,
            desired_revision: 0,
            applied_revision: 0,
            desired_digest: String::new(),
            applied_digest: String::new(),
            artifact_sha256: String::new(),
            listen_port: 0,
            apply_status: "not_prepared".into(),
            desired_running: false,
            public_origin: String::new(),
            owned_origin: String::new(),
            policy_ready: false,
            unavailable: false,
            desired_auth: Vec::new(),
            applied_auth: Vec::new(),
            desired_routes: Vec::new(),
            applied_routes: Vec::new(),
            host_capabilities: Vec::new(),
            oauth: Vec::new(),
            previous_accepted: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRecord {
    version: u32,
    child_generation: String,
    applied_generation: String,
    desired_revision: String,
    applied_revision: String,
    desired_digest: String,
    applied_digest: String,
    artifact_sha256: String,
    listen_port: u16,
    apply_status: String,
    desired_running: bool,
    public_origin: String,
    owned_origin: String,
    policy_ready: bool,
    unavailable: bool,
    desired_auth: Vec<RawAuth>,
    applied_auth: Vec<RawAuth>,
    #[serde(default, alias = "desired_routes")]
    desired_routes: Vec<RawRouteSet>,
    #[serde(default, alias = "applied_routes")]
    applied_routes: Vec<RawRouteSet>,
    #[serde(default)]
    host_capabilities: Vec<String>,
    oauth: Vec<RawOAuth>,
    #[serde(default)]
    previous_accepted: Option<RawSnapshot>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSnapshot {
    generation: String,
    revision: String,
    wire_digest: String,
    auth: Vec<RawAuth>,
    routes: Vec<RawRouteSet>,
    artifact_sha256: String,
    listen_port: u16,
    owned_origin: String,
    public_origin: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAuth {
    auth_id: String,
    credential_id: String,
    credential_version: String,
    binding_id: String,
    material_revision: String,
    provider_id: String,
    registration_epoch: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOAuth {
    relative_path: String,
    auth_id: String,
    credential_id: String,
    credential_version: String,
    material_revision: String,
    provider_id: String,
    #[serde(default)]
    native_provider: String,
    registration_epoch: String,
    models: Vec<String>,
    #[serde(default = "presence_present")]
    presence: String,
    #[serde(default)]
    recovery: String,
    #[serde(default)]
    raw_provider_label: String,
    #[serde(default)]
    native_mode: String,
    #[serde(default)]
    reported_base: String,
}

fn presence_present() -> String {
    "present".to_string()
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRouteSet {
    auth_id: String,
    credential_id: String,
    credential_version: String,
    binding_id: String,
    material_fingerprint: String,
    routing_rank: u32,
    routes: Vec<RawNormalizedRoute>,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawNormalizedRoute {
    public_model: String,
    upstream_model: String,
    protocol: String,
    endpoint_id: String,
    origin: String,
    endpoint_fingerprint: String,
    validation_only: bool,
    #[serde(default)]
    native_targets: Vec<NativeDispatchTarget>,
}

pub(super) fn load(conn: &Connection) -> Result<Option<Record>, ()> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    match value {
        None => Ok(None),
        Some(json) => parse(&json).map(Some),
    }
}

pub(super) fn load_tx(tx: &rusqlite::Transaction<'_>) -> Result<Record, ()> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [SETTINGS_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| ())?;
    match value {
        None => Ok(Record::empty()),
        Some(json) => parse(&json),
    }
}

pub(super) fn save(conn: &Connection, record: &Record) -> Result<(), ExecutionError> {
    let json = serde_json::to_string(&raw_from(record))
        .map_err(|_| ExecutionError::Invalid("CPA execution record could not be encoded".into()))?;
    if json.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "CPA execution record is too large".into(),
        ));
    }
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![SETTINGS_KEY, json],
    )
    .map_err(|_| ExecutionError::Unavailable("CPA execution record could not be saved".into()))?;
    Ok(())
}

fn parse(json: &str) -> Result<Record, ()> {
    if json.len() > MAX_CONFIG_BYTES {
        return Err(());
    }
    let raw: RawRecord = serde_json::from_str(json).map_err(|_| ())?;
    if raw.version != RECORD_VERSION || !status_ok(&raw.apply_status) {
        return Err(());
    }
    if !digest_ok(&raw.desired_digest) || !digest_ok(&raw.applied_digest) {
        return Err(());
    }
    if !raw.artifact_sha256.is_empty() && !digest_ok(&raw.artifact_sha256) {
        return Err(());
    }
    Ok(Record {
        child_generation: decimal(&raw.child_generation)?,
        applied_generation: decimal(&raw.applied_generation)?,
        desired_revision: decimal(&raw.desired_revision)?,
        applied_revision: decimal(&raw.applied_revision)?,
        desired_digest: raw.desired_digest.to_ascii_lowercase(),
        applied_digest: raw.applied_digest.to_ascii_lowercase(),
        artifact_sha256: raw.artifact_sha256.to_ascii_lowercase(),
        listen_port: raw.listen_port,
        apply_status: raw.apply_status,
        desired_running: raw.desired_running,
        public_origin: raw.public_origin,
        owned_origin: raw.owned_origin,
        policy_ready: raw.policy_ready,
        unavailable: raw.unavailable,
        desired_auth: raw
            .desired_auth
            .into_iter()
            .map(auth_from)
            .collect::<Result<_, _>>()?,
        applied_auth: raw
            .applied_auth
            .into_iter()
            .map(auth_from)
            .collect::<Result<_, _>>()?,
        desired_routes: raw
            .desired_routes
            .into_iter()
            .map(route_from)
            .collect::<Result<_, _>>()?,
        applied_routes: raw
            .applied_routes
            .into_iter()
            .map(route_from)
            .collect::<Result<_, _>>()?,
        host_capabilities: raw.host_capabilities,
        oauth: raw
            .oauth
            .into_iter()
            .map(oauth_from)
            .collect::<Result<_, _>>()?,
        previous_accepted: raw.previous_accepted.map(snapshot_from).transpose()?,
    })
}

fn raw_from(record: &Record) -> RawRecord {
    RawRecord {
        version: RECORD_VERSION,
        child_generation: record.child_generation.to_string(),
        applied_generation: record.applied_generation.to_string(),
        desired_revision: record.desired_revision.to_string(),
        applied_revision: record.applied_revision.to_string(),
        desired_digest: record.desired_digest.clone(),
        applied_digest: record.applied_digest.clone(),
        artifact_sha256: record.artifact_sha256.clone(),
        listen_port: record.listen_port,
        apply_status: record.apply_status.clone(),
        desired_running: record.desired_running,
        public_origin: record.public_origin.clone(),
        owned_origin: record.owned_origin.clone(),
        policy_ready: record.policy_ready,
        unavailable: record.unavailable,
        desired_auth: record.desired_auth.iter().map(auth_raw).collect(),
        applied_auth: record.applied_auth.iter().map(auth_raw).collect(),
        desired_routes: record.desired_routes.iter().map(route_raw).collect(),
        applied_routes: record.applied_routes.iter().map(route_raw).collect(),
        host_capabilities: record.host_capabilities.clone(),
        oauth: record.oauth.iter().map(oauth_raw).collect(),
        previous_accepted: record.previous_accepted.as_ref().map(snapshot_raw),
    }
}

fn auth_from(raw: RawAuth) -> Result<AuthStamp, ()> {
    Ok(AuthStamp {
        auth_id: raw.auth_id,
        credential_id: raw.credential_id,
        credential_version: decimal(&raw.credential_version)?,
        binding_id: raw.binding_id,
        material_revision: raw.material_revision,
        provider_id: raw.provider_id,
        registration_epoch: decimal(&raw.registration_epoch)?,
    })
}

fn auth_raw(stamp: &AuthStamp) -> RawAuth {
    RawAuth {
        auth_id: stamp.auth_id.clone(),
        credential_id: stamp.credential_id.clone(),
        credential_version: stamp.credential_version.to_string(),
        binding_id: stamp.binding_id.clone(),
        material_revision: stamp.material_revision.clone(),
        provider_id: stamp.provider_id.clone(),
        registration_epoch: stamp.registration_epoch.to_string(),
    }
}

fn oauth_from(raw: RawOAuth) -> Result<OAuthStamp, ()> {
    if !native_mode_ok(&raw.native_mode) {
        return Err(());
    }
    let (provider_id, native_provider) = split_provider(&raw.provider_id, &raw.native_provider)?;
    Ok(OAuthStamp {
        relative_path: raw.relative_path,
        auth_id: raw.auth_id,
        credential_id: raw.credential_id,
        credential_version: decimal(&raw.credential_version)?,
        material_revision: raw.material_revision,
        provider_id,
        native_provider,
        registration_epoch: decimal(&raw.registration_epoch)?,
        models: raw.models,
        presence: presence_from(&raw.presence)?,
        recovery: raw.recovery,
        raw_provider_label: raw.raw_provider_label,
        native_mode: raw.native_mode,
        reported_base: raw.reported_base,
    })
}

fn split_provider(provider_id: &str, native_provider: &str) -> Result<(String, String), ()> {
    let native = native_provider.trim();
    let product = provider_id.trim();
    if native.is_empty() && super::native::known_provider(product) {
        return Ok(("cpa".to_string(), product.to_string()));
    }
    if native.is_empty() {
        return Ok((
            if product.is_empty() {
                "cpa".to_string()
            } else {
                product.to_string()
            },
            String::new(),
        ));
    }
    if !super::native::known_provider(native) {
        return Err(());
    }
    let product = if product.is_empty() || super::native::known_provider(product) {
        "cpa".to_string()
    } else {
        product.to_string()
    };
    Ok((product, native.to_string()))
}

fn presence_from(value: &str) -> Result<OAuthPresence, ()> {
    match value {
        "" | "present" => Ok(OAuthPresence::Present),
        "absent" => Ok(OAuthPresence::Absent),
        "pending" => Ok(OAuthPresence::Pending),
        _ => Err(()),
    }
}

fn oauth_raw(stamp: &OAuthStamp) -> RawOAuth {
    RawOAuth {
        relative_path: stamp.relative_path.clone(),
        auth_id: stamp.auth_id.clone(),
        credential_id: stamp.credential_id.clone(),
        credential_version: stamp.credential_version.to_string(),
        material_revision: stamp.material_revision.clone(),
        provider_id: stamp.provider_id.clone(),
        native_provider: stamp.native_provider.clone(),
        registration_epoch: stamp.registration_epoch.to_string(),
        models: stamp.models.clone(),
        presence: match stamp.presence {
            OAuthPresence::Present => "present".to_string(),
            OAuthPresence::Absent => "absent".to_string(),
            OAuthPresence::Pending => "pending".to_string(),
        },
        recovery: stamp.recovery.clone(),
        raw_provider_label: stamp.raw_provider_label.clone(),
        native_mode: stamp.native_mode.clone(),
        reported_base: stamp.reported_base.clone(),
    }
}

fn route_from(raw: RawRouteSet) -> Result<CredentialRouteSet, ()> {
    Ok(CredentialRouteSet {
        auth_id: raw.auth_id,
        credential_id: raw.credential_id,
        credential_version: decimal(&raw.credential_version)?,
        binding_id: raw.binding_id,
        material_fingerprint: raw.material_fingerprint,
        routing_rank: raw.routing_rank,
        routes: raw
            .routes
            .into_iter()
            .map(normalized_from)
            .collect::<Result<Vec<_>, _>>()?,
        fingerprint: raw.fingerprint,
    })
}

fn normalized_from(raw: RawNormalizedRoute) -> Result<NormalizedRoute, ()> {
    if !accept_stored_native_targets(&raw.native_targets) {
        return Err(());
    }
    Ok(NormalizedRoute {
        public_model: raw.public_model,
        upstream_model: raw.upstream_model,
        protocol: raw.protocol,
        endpoint_id: raw.endpoint_id,
        origin: raw.origin,
        endpoint_fingerprint: raw.endpoint_fingerprint,
        validation_only: raw.validation_only,
        native_targets: raw.native_targets,
    })
}

fn route_raw(set: &CredentialRouteSet) -> RawRouteSet {
    RawRouteSet {
        auth_id: set.auth_id.clone(),
        credential_id: set.credential_id.clone(),
        credential_version: set.credential_version.to_string(),
        binding_id: set.binding_id.clone(),
        material_fingerprint: set.material_fingerprint.clone(),
        routing_rank: set.routing_rank,
        routes: set.routes.iter().map(normalized_raw).collect(),
        fingerprint: set.fingerprint.clone(),
    }
}

fn normalized_raw(route: &NormalizedRoute) -> RawNormalizedRoute {
    RawNormalizedRoute {
        public_model: route.public_model.clone(),
        upstream_model: route.upstream_model.clone(),
        protocol: route.protocol.clone(),
        endpoint_id: route.endpoint_id.clone(),
        origin: route.origin.clone(),
        endpoint_fingerprint: route.endpoint_fingerprint.clone(),
        validation_only: route.validation_only,
        native_targets: route.native_targets.clone(),
    }
}

fn snapshot_from(raw: RawSnapshot) -> Result<AcceptedProjectionSnapshot, ()> {
    let generation = decimal(&raw.generation)?;
    let revision = decimal(&raw.revision)?;
    if generation == 0 || revision == 0 || raw.listen_port == 0 {
        return Err(());
    }
    if !exact_digest(&raw.wire_digest) || !exact_digest(&raw.artifact_sha256) {
        return Err(());
    }
    if raw.owned_origin.is_empty()
        || raw.public_origin.is_empty()
        || raw.owned_origin == raw.public_origin
    {
        return Err(());
    }
    let auth = raw
        .auth
        .into_iter()
        .map(auth_from)
        .collect::<Result<Vec<_>, _>>()?;
    let routes = raw
        .routes
        .into_iter()
        .map(route_from)
        .collect::<Result<Vec<_>, _>>()?;
    if !maps_coherent(&auth, &routes) {
        return Err(());
    }
    Ok(AcceptedProjectionSnapshot {
        generation,
        revision,
        wire_digest: raw.wire_digest.to_ascii_lowercase(),
        auth,
        routes,
        artifact_sha256: raw.artifact_sha256.to_ascii_lowercase(),
        listen_port: raw.listen_port,
        owned_origin: raw.owned_origin,
        public_origin: raw.public_origin,
    })
}

fn snapshot_raw(snapshot: &AcceptedProjectionSnapshot) -> RawSnapshot {
    RawSnapshot {
        generation: snapshot.generation.to_string(),
        revision: snapshot.revision.to_string(),
        wire_digest: snapshot.wire_digest.clone(),
        auth: snapshot.auth.iter().map(auth_raw).collect(),
        routes: snapshot.routes.iter().map(route_raw).collect(),
        artifact_sha256: snapshot.artifact_sha256.clone(),
        listen_port: snapshot.listen_port,
        owned_origin: snapshot.owned_origin.clone(),
        public_origin: snapshot.public_origin.clone(),
    }
}

fn exact_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn maps_coherent(auth: &[AuthStamp], routes: &[CredentialRouteSet]) -> bool {
    let mut auth_ids = BTreeSet::new();
    let mut credentials = BTreeSet::new();
    for stamp in auth {
        if stamp.auth_id.is_empty() || stamp.credential_id.is_empty() || stamp.binding_id.is_empty()
        {
            return false;
        }
        if !auth_ids.insert(stamp.auth_id.as_str()) {
            return false;
        }
        if !credentials.insert((
            stamp.credential_id.as_str(),
            stamp.credential_version,
            stamp.binding_id.as_str(),
        )) {
            return false;
        }
    }
    let mut route_ids = BTreeSet::new();
    for set in routes {
        if set.auth_id.is_empty() || set.credential_id.is_empty() || set.binding_id.is_empty() {
            return false;
        }
        if !route_ids.insert(set.auth_id.as_str()) {
            return false;
        }
        if let Some(stamp) = auth.iter().find(|stamp| stamp.auth_id == set.auth_id) {
            if stamp.credential_id != set.credential_id
                || stamp.credential_version != set.credential_version
                || stamp.binding_id != set.binding_id
                || stamp.material_revision != set.material_fingerprint
            {
                return false;
            }
        }
    }
    true
}

pub(super) fn snapshot_matching_applied(
    record: &Record,
    yaml: &str,
) -> Option<AcceptedProjectionSnapshot> {
    if record.apply_status != "applied" || !record.policy_ready {
        return None;
    }
    let proof = yaml_proof(yaml)?;
    if !tuple_matches(
        record.applied_generation,
        record.applied_revision,
        &record.applied_digest,
        record.listen_port,
        &record.owned_origin,
        &record.public_origin,
        &record.artifact_sha256,
        &proof,
    ) || !maps_coherent(&record.applied_auth, &record.applied_routes)
    {
        return None;
    }
    Some(AcceptedProjectionSnapshot {
        generation: record.applied_generation,
        revision: record.applied_revision,
        wire_digest: record.applied_digest.clone(),
        auth: record.applied_auth.clone(),
        routes: record.applied_routes.clone(),
        artifact_sha256: record.artifact_sha256.clone(),
        listen_port: record.listen_port,
        owned_origin: record.owned_origin.clone(),
        public_origin: record.public_origin.clone(),
    })
}

pub(super) fn applied_matches_yaml(record: &Record, yaml: &str) -> bool {
    let Some(proof) = yaml_proof(yaml) else {
        return false;
    };
    tuple_matches(
        record.applied_generation,
        record.applied_revision,
        &record.applied_digest,
        record.listen_port,
        &record.owned_origin,
        &record.public_origin,
        &record.artifact_sha256,
        &proof,
    ) && maps_coherent(&record.applied_auth, &record.applied_routes)
}

pub(super) fn snapshot_matches_yaml(snapshot: &AcceptedProjectionSnapshot, yaml: &str) -> bool {
    let Some(proof) = yaml_proof(yaml) else {
        return false;
    };
    tuple_matches(
        snapshot.generation,
        snapshot.revision,
        &snapshot.wire_digest,
        snapshot.listen_port,
        &snapshot.owned_origin,
        &snapshot.public_origin,
        &snapshot.artifact_sha256,
        &proof,
    )
}

struct YamlProof {
    generation: u64,
    revision: u64,
    digest: String,
    listen_port: u16,
    owned_origin: String,
    public_origin: String,
}

fn tuple_matches(
    generation: u64,
    revision: u64,
    digest: &str,
    listen_port: u16,
    owned_origin: &str,
    public_origin: &str,
    artifact_sha256: &str,
    proof: &YamlProof,
) -> bool {
    generation != 0
        && revision != 0
        && exact_digest(digest)
        && exact_digest(artifact_sha256)
        && generation == proof.generation
        && revision == proof.revision
        && digest.eq_ignore_ascii_case(&proof.digest)
        && listen_port == proof.listen_port
        && owned_origin == proof.owned_origin
        && public_origin == proof.public_origin
}

fn yaml_proof(yaml: &str) -> Option<YamlProof> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).ok()?;
    let generation = yaml_decimal(value.get("ocg")?.get("process-generation"))?;
    let revision = yaml_decimal(value.get("ocg")?.get("projection-revision"))?;
    let listen_port = yaml_port(value.get("port"))?;
    let public_origin = value
        .get("ocg")?
        .get("policy")?
        .get("origin")?
        .as_str()?
        .to_string();
    if generation == 0 || revision == 0 || listen_port == 0 || public_origin.is_empty() {
        return None;
    }
    let owned_origin = format!("http://127.0.0.1:{listen_port}");
    if owned_origin == public_origin {
        return None;
    }
    Some(YamlProof {
        generation,
        revision,
        digest: crate::cpa_projection::wire_digest(yaml),
        listen_port,
        owned_origin,
        public_origin,
    })
}

fn yaml_decimal(value: Option<&serde_yaml_ng::Value>) -> Option<u64> {
    match value? {
        serde_yaml_ng::Value::String(text) => decimal(text).ok(),
        serde_yaml_ng::Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

fn yaml_port(value: Option<&serde_yaml_ng::Value>) -> Option<u16> {
    let port = match value? {
        serde_yaml_ng::Value::Number(number) => number.as_u64()?,
        serde_yaml_ng::Value::String(text) => decimal(text).ok()?,
        _ => return None,
    };
    u16::try_from(port).ok()
}

/// Rewrite digests of YAML files that already match an accepted tuple.
///
/// A desired plane that is not the applied plane is left untouched. Unknown
/// YAML returns no edit. A record that cannot be parsed is refused.
pub(super) fn rebase_archived_execution_record(
    json: &str,
    current_before: &str,
    current_after: &str,
    previous_before: Option<&str>,
    previous_after: Option<&str>,
) -> Result<Option<String>, ExecutionError> {
    if json.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "CPA execution record is too large".into(),
        ));
    }
    if parse(json).is_err() {
        return Err(ExecutionError::Invalid(
            "CPA execution record could not be read".into(),
        ));
    }
    let mut value: Value = serde_json::from_str(json)
        .map_err(|_| ExecutionError::Invalid("CPA execution record could not be read".into()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| ExecutionError::Invalid("CPA execution record could not be read".into()))?;
    let mut changed = false;
    if let Some(proof) = yaml_proof(current_before) {
        if json_tuple_matches(
            object,
            "appliedGeneration",
            "appliedRevision",
            "appliedDigest",
            &proof,
        ) {
            let after = yaml_proof(current_after).ok_or_else(|| {
                ExecutionError::Invalid("relocated CPA config could not be read".into())
            })?;
            object.insert(
                "appliedDigest".to_string(),
                Value::String(after.digest.clone()),
            );
            if desired_is_same_accepted(object, &proof) {
                object.insert(
                    "desiredDigest".to_string(),
                    Value::String(after.digest.clone()),
                );
            }
            object.insert(
                "listenPort".to_string(),
                Value::from(u64::from(after.listen_port)),
            );
            object.insert(
                "ownedOrigin".to_string(),
                Value::String(after.owned_origin.clone()),
            );
            object.insert(
                "publicOrigin".to_string(),
                Value::String(after.public_origin.clone()),
            );
            changed = true;
        }
    }
    if let (Some(before), Some(after_yaml)) = (previous_before, previous_after) {
        if let Some(proof) = yaml_proof(before) {
            if let Some(snapshot) = object
                .get_mut("previousAccepted")
                .and_then(Value::as_object_mut)
            {
                if json_tuple_matches(snapshot, "generation", "revision", "wireDigest", &proof) {
                    let after = yaml_proof(after_yaml).ok_or_else(|| {
                        ExecutionError::Invalid("relocated CPA config could not be read".into())
                    })?;
                    snapshot.insert("wireDigest".to_string(), Value::String(after.digest));
                    snapshot.insert(
                        "listenPort".to_string(),
                        Value::from(u64::from(after.listen_port)),
                    );
                    snapshot.insert("ownedOrigin".to_string(), Value::String(after.owned_origin));
                    snapshot.insert(
                        "publicOrigin".to_string(),
                        Value::String(after.public_origin),
                    );
                    changed = true;
                }
            }
        }
    }
    finish_rebase(changed, &value)
}

fn finish_rebase(changed: bool, value: &Value) -> Result<Option<String>, ExecutionError> {
    if !changed {
        return Ok(None);
    }
    let json = serde_json::to_string(value)
        .map_err(|_| ExecutionError::Invalid("CPA execution record could not be encoded".into()))?;
    if json.len() > MAX_CONFIG_BYTES {
        return Err(ExecutionError::Invalid(
            "CPA execution record is too large".into(),
        ));
    }
    if parse(&json).is_err() {
        return Err(ExecutionError::Invalid(
            "CPA execution record could not be read".into(),
        ));
    }
    Ok(Some(json))
}

fn desired_is_same_accepted(object: &serde_json::Map<String, Value>, proof: &YamlProof) -> bool {
    json_decimal(object.get("desiredRevision")) == Some(proof.revision)
        && json_decimal(object.get("childGeneration")) == Some(proof.generation)
        && object
            .get("desiredDigest")
            .and_then(Value::as_str)
            .is_some_and(|digest| digest.eq_ignore_ascii_case(&proof.digest))
}

fn json_tuple_matches(
    object: &serde_json::Map<String, Value>,
    generation_key: &str,
    revision_key: &str,
    digest_key: &str,
    proof: &YamlProof,
) -> bool {
    let generation = json_decimal(object.get(generation_key));
    let revision = json_decimal(object.get(revision_key));
    let digest = object.get(digest_key).and_then(Value::as_str);
    let listen_port = object.get("listenPort").and_then(Value::as_u64);
    let owned = object.get("ownedOrigin").and_then(Value::as_str);
    let public = object.get("publicOrigin").and_then(Value::as_str);
    let artifact = object.get("artifactSha256").and_then(Value::as_str);
    generation == Some(proof.generation)
        && revision == Some(proof.revision)
        && digest.is_some_and(|value| value.eq_ignore_ascii_case(&proof.digest))
        && listen_port == Some(u64::from(proof.listen_port))
        && owned == Some(proof.owned_origin.as_str())
        && public == Some(proof.public_origin.as_str())
        && artifact.is_some_and(exact_digest)
}

fn json_decimal(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::String(text) => decimal(text).ok(),
        Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

fn status_ok(value: &str) -> bool {
    matches!(
        value,
        "not_prepared" | "installed" | "apply_pending" | "applied" | "apply_failed" | "stopped"
    )
}

fn digest_ok(value: &str) -> bool {
    value.is_empty() || (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests;

pub(super) fn decimal(value: &str) -> Result<u64, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(());
    }
    value.parse().map_err(|_| ())
}
