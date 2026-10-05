//! One private config.yaml from the product projection.
//! A historical remote CPA base is omitted. It is not an upstream.

use super::ExecutionError;
use super::io::{self};
use super::store::{AuthStamp, OAuthPresence, OAuthStamp, Record};
use crate::cpa_projection::{
    CredentialRouteSet, NativeInclusion, NormalizedRoute, OAuthFileRef, PendingApprovedRoute,
    ProjectionInput, ProjectionRevisions, RuntimeEnvelope, ValidationCandidate,
    ValidationCandidateKind, ValidationRow, ValidationSidecar, classify_validation,
    credential_route_fingerprint, native_inclusion, note_native_route_unmapped, project,
    refresh_logical_digest, registration_is_complete, render_canonical_yaml,
};
use crate::db::Database;
use crate::models::AppConfig;
use crate::provider::builtin_provider;
use crate::provider_contracts::{ContractScope, ProtocolOverrideState, build_effective_contracts};
use crate::routing_snapshot::RoutingSnapshot;
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::credential::ModelScope;
use ocg_domain::destination::{AdapterKind, LegacyDestinationRef};
use ocg_infra::crypto::KeyCipher;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub(super) struct RenderedProjection {
    pub yaml: String,
    pub digest: String,
    pub desired_auth: Vec<AuthStamp>,
    pub oauth: Vec<OAuthStamp>,
    /// API route sets plus one set per emitted native ref.
    pub routes: Vec<CredentialRouteSet>,
}

pub(super) fn render(
    db: &Database,
    snapshot: &RoutingSnapshot,
    config: &AppConfig,
    record: &Record,
    child_generation: u64,
    desired_revision: u64,
    port: u16,
    public_origin: &str,
    child_origin: &str,
    secrets: &super::Secrets,
    cipher: &dyn KeyCipher,
    data_dir: &Path,
) -> Result<RenderedProjection, ExecutionError> {
    reject_self_loop(snapshot, public_origin)?;
    let preset_ids = preset_ids(db)?;
    let auth_dir = io::portable_auth_dir(data_dir)?;
    io::ensure_dir(&io::auth_dir(data_dir))?;
    let oauth = usable_oauth(record, data_dir);
    let facts = load_credential_facts(db)?;
    let pending = pending_by_credential(db, snapshot, &facts)?;
    let candidates = validation_candidates(&facts, &pending);
    let refs = oauth
        .iter()
        .filter_map(|stamp| oauth_ref(stamp, &facts))
        .collect::<Vec<_>>();
    let owned_ids = owned_destination_ids(snapshot, child_origin, &record.owned_origin);
    let owned_refs = owned_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let mut extra = Vec::new();
    if !record.owned_origin.is_empty() && record.owned_origin != child_origin {
        extra.push(record.owned_origin.as_str());
    }
    let policy_url = format!("{public_origin}/_internal/ocg/cpa-policy");
    let input = ProjectionInput {
        snapshot,
        config,
        cipher,
        revisions: ProjectionRevisions {
            desired: desired_revision,
            applied: record.applied_revision,
        },
        owned_listener: child_origin,
        owned_origins: &extra,
        owned_destination_ids: &owned_refs,
        oauth_refs: &refs,
        preset_ids: &preset_ids,
        validation: ValidationSidecar {
            candidates: &candidates,
        },
        runtime: RuntimeEnvelope {
            process_generation: child_generation,
            port,
            auth_dir: &auth_dir,
            policy_url: &policy_url,
            policy_origin: public_origin,
            ready_key: secrets.ready.expose(),
            hop_secret: secrets.hop.expose(),
            policy_token: secrets.policy.expose(),
        },
    };
    let mut projection =
        project(input).map_err(|error| ExecutionError::Invalid(error.code.to_string()))?;
    let native = native_route_sets(&projection.oauth_refs, &facts);
    let covered = native
        .iter()
        .map(|set| {
            (
                set.auth_id.clone(),
                set.credential_id.clone(),
                set.credential_version,
            )
        })
        .collect::<Vec<_>>();
    let unmapped = projection
        .oauth_refs
        .iter()
        .filter(|item| {
            !covered.iter().any(|(auth_id, credential_id, version)| {
                auth_id == &item.auth_id
                    && credential_id == &item.credential_id
                    && *version == item.credential_version
            })
        })
        .map(|item| item.credential_id.clone())
        .collect::<Vec<_>>();
    projection.oauth_refs.retain(|item| {
        covered.iter().any(|(auth_id, credential_id, version)| {
            auth_id == &item.auth_id
                && credential_id == &item.credential_id
                && *version == item.credential_version
        })
    });
    for credential_id in unmapped {
        note_native_route_unmapped(&mut projection, credential_id);
    }
    projection.route_sets.extend(native);
    refresh_logical_digest(&mut projection)
        .map_err(|error| ExecutionError::Invalid(error.code.to_string()))?;
    let rendered = render_canonical_yaml(&projection)
        .map_err(|error| ExecutionError::Invalid(error.code.to_string()))?;
    let desired_auth = projection
        .auths
        .iter()
        .map(|auth| {
            let previous = record
                .applied_auth
                .iter()
                .chain(record.desired_auth.iter())
                .find(|stamp| {
                    stamp.credential_id == auth.credential_id
                        && stamp.credential_version == auth.credential_version
                });
            let oauth_stamp = oauth
                .iter()
                .find(|stamp| stamp.credential_id == auth.credential_id);
            let material_revision = auth
                .material
                .as_ref()
                .map(|material| material.fingerprint())
                .or_else(|| oauth_stamp.map(|stamp| stamp.material_revision.clone()))
                .unwrap_or_else(|| "no-material".to_string());
            AuthStamp {
                auth_id: auth.auth_id.clone(),
                credential_id: auth.credential_id.clone(),
                credential_version: auth.credential_version,
                binding_id: auth.binding_id.clone(),
                material_revision,
                provider_id: auth.provenance.provider_id.clone(),
                registration_epoch: previous.map(|stamp| stamp.registration_epoch).unwrap_or(0),
            }
        })
        .collect();
    Ok(RenderedProjection {
        yaml: rendered.yaml,
        digest: rendered.wire_digest,
        desired_auth,
        oauth,
        routes: projection.route_sets,
    })
}

pub(super) fn reject_self_loop(
    snapshot: &RoutingSnapshot,
    public_origin: &str,
) -> Result<(), ExecutionError> {
    for destination in &snapshot.projection.destinations {
        if destination.adapter != AdapterKind::Cpa {
            continue;
        }
        let Some(base) = destination.base_url.as_deref() else {
            continue;
        };
        if origin_of(base).as_deref() == Some(public_origin) {
            return Err(ExecutionError::Invalid("cpa_self_loop".into()));
        }
    }
    Ok(())
}

pub(super) fn owned_destination_ids(
    snapshot: &RoutingSnapshot,
    child_origin: &str,
    previous_owned: &str,
) -> Vec<String> {
    snapshot
        .projection
        .destinations
        .iter()
        .filter(|destination| destination.adapter == AdapterKind::Cpa)
        .filter(|destination| {
            destination
                .base_url
                .as_deref()
                .and_then(|base| origin_of(base))
                .is_some_and(|origin| origin == child_origin || origin == previous_owned)
        })
        .map(|destination| destination.id.clone())
        .collect()
}

pub(super) fn origin_of(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(format!("{}://{host}", scheme.to_ascii_lowercase()))
}

fn preset_ids(db: &Database) -> Result<BTreeMap<String, String>, ExecutionError> {
    let mut statement = db
        .conn
        .prepare("SELECT id, preset_id FROM destinations")
        .map_err(|_| ExecutionError::Unavailable("destination presets could not be read".into()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|_| ExecutionError::Unavailable("destination presets could not be read".into()))?;
    let mut presets = BTreeMap::new();
    for row in rows {
        let (id, preset) = row.map_err(|_| {
            ExecutionError::Unavailable("destination presets could not be read".into())
        })?;
        if let Some(preset) = preset.filter(|value| !value.is_empty()) {
            presets.insert(id, preset);
        }
    }
    Ok(presets)
}

fn usable_oauth(record: &Record, data_dir: &Path) -> Vec<OAuthStamp> {
    record
        .oauth
        .iter()
        .filter(|stamp| stamp.presence == OAuthPresence::Present && oauth_file_ok(data_dir, stamp))
        .cloned()
        .collect()
}

fn oauth_file_ok(data_dir: &Path, stamp: &OAuthStamp) -> bool {
    if stamp.models.is_empty()
        || stamp.material_revision.is_empty()
        || stamp.credential_id.is_empty()
        || stamp.credential_version == 0
        || !safe_segment(&stamp.relative_path)
    {
        return false;
    }
    let path = io::auth_dir(data_dir).join(&stamp.relative_path);
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return false;
    };
    metadata.file_type().is_file() && !metadata.file_type().is_symlink()
}

fn safe_segment(value: &str) -> bool {
    !value.contains('/')
        && !value.contains('\\')
        && value != "."
        && value != ".."
        && !value.is_empty()
}

/// Rank stored on the credential row. Disabled rows are omitted. Zero is a real
/// earliest rank, so a missing or disabled row is `None` rather than `Some(0)`.
/// A validation-only native row keeps its stored rank. This does not read a token.
pub(super) fn oauth_routing_rank(db: &Database, credential_id: &str) -> Option<u32> {
    let facts = load_credential_facts(db).ok()?;
    let fact = facts
        .iter()
        .find(|fact| fact.credential_id == credential_id)?;
    native_rank(fact)
}

fn oauth_ref(stamp: &OAuthStamp, facts: &[CredentialFact]) -> Option<OAuthFileRef> {
    if stamp.presence != OAuthPresence::Present
        || stamp.provider_id != crate::provider::CPA_PROVIDER_ID
    {
        return None;
    }
    let provider = super::native::provider_from_label(&stamp.native_provider)?;
    let fact = facts
        .iter()
        .find(|fact| fact.credential_id == stamp.credential_id)?;
    let routing_rank = native_rank(fact)?;
    Some(OAuthFileRef {
        provider,
        product_provider_id: stamp.provider_id.clone(),
        relative_path: stamp.relative_path.clone(),
        material_revision: stamp.material_revision.clone(),
        auth_id: stamp.auth_id.clone(),
        credential_id: stamp.credential_id.clone(),
        credential_version: stamp.credential_version,
        routing_rank,
        models: stamp.models.clone(),
        raw_provider_label: stamp.raw_provider_label.clone(),
        native_mode: stamp.native_mode.clone(),
        reported_base: stamp.reported_base.clone(),
    })
}

struct CredentialFact {
    credential_id: String,
    credential_version: u64,
    routing_rank: Option<u32>,
    binding_id: String,
    enabled: bool,
    binding_enabled: bool,
    setup_step: String,
    account_type: String,
    material_present: bool,
    destination_id: String,
    destination_enabled: bool,
    destination_draft: bool,
    scope_json: String,
}

fn unavailable(message: &str) -> ExecutionError {
    ExecutionError::Unavailable(message.to_string())
}

fn load_credential_facts(db: &Database) -> Result<Vec<CredentialFact>, ExecutionError> {
    let mut statement = db
        .conn
        .prepare(
            "SELECT c.id, c.credential_version, c.routing_rank, c.enabled,
                    COALESCE(c.binding_enabled, 1), COALESCE(c.binding_id, ''),
                    COALESCE(c.setup_step, ''), COALESCE(c.account_type, 'key'), c.key_cipher,
                    COALESCE(c.destination_id, ''),
                    COALESCE(d.enabled, 0), COALESCE(d.onboarding_draft, 0),
                    COALESCE(c.scope_json, '')
             FROM credentials c
             LEFT JOIN destinations d ON d.id = c.destination_id
             WHERE COALESCE(c.credential_purpose, 'inference') = 'inference'",
        )
        .map_err(|_| unavailable("validation facts could not be read"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, String>(12)?,
            ))
        })
        .map_err(|_| unavailable("validation facts could not be read"))?;
    let mut facts = Vec::new();
    for row in rows {
        let (
            credential_id,
            version,
            rank,
            enabled,
            binding_enabled,
            binding_id,
            setup_step,
            account_type,
            key_cipher,
            destination_id,
            destination_enabled,
            destination_draft,
            scope_json,
        ) = row.map_err(|_| unavailable("validation facts could not be read"))?;
        let Some(version) = version.filter(|value| *value > 0) else {
            continue;
        };
        let Ok(credential_version) = u64::try_from(version) else {
            continue;
        };
        facts.push(CredentialFact {
            credential_id,
            credential_version,
            routing_rank: rank.and_then(|value| u32::try_from(value).ok()),
            binding_id,
            enabled: enabled.unwrap_or(0) != 0,
            binding_enabled: binding_enabled != 0,
            setup_step,
            account_type,
            material_present: !key_cipher.trim().is_empty(),
            destination_id,
            destination_enabled: destination_enabled != 0,
            destination_draft: destination_draft != 0,
            scope_json,
        });
    }
    Ok(facts)
}

fn validation_row(fact: &CredentialFact) -> ValidationRow {
    ValidationRow {
        credential_id: fact.credential_id.clone(),
        credential_version: fact.credential_version,
        enabled: fact.enabled,
        binding_enabled: fact.binding_enabled,
        setup_step: fact.setup_step.clone(),
        account_type: fact.account_type.clone(),
        material_present: fact.material_present,
        destination_enabled: fact.destination_enabled,
        destination_draft: fact.destination_draft,
        registration_complete: registration_is_complete(&fact.account_type, &fact.setup_step),
    }
}

fn native_rank(fact: &CredentialFact) -> Option<u32> {
    native_inclusion(&validation_row(fact))?;
    fact.routing_rank
}

fn validation_candidates(
    facts: &[CredentialFact],
    pending: &BTreeMap<String, Vec<PendingApprovedRoute>>,
) -> Vec<ValidationCandidate> {
    let mut candidates = Vec::new();
    for fact in facts {
        let Some(kind) = classify_validation(&validation_row(fact)) else {
            continue;
        };
        let routes = pending
            .get(&fact.credential_id)
            .cloned()
            .unwrap_or_default();
        let emit = match kind {
            ValidationCandidateKind::ManagedKeyVerification => true,
            ValidationCandidateKind::CompletePending => {
                fact.destination_draft || !routes.is_empty()
            }
        };
        if !emit {
            continue;
        }
        candidates.push(ValidationCandidate {
            credential_id: fact.credential_id.clone(),
            credential_version: fact.credential_version,
            kind,
            setup_step: fact.setup_step.clone(),
            account_type: fact.account_type.clone(),
            material_present: fact.material_present,
            destination_draft: fact.destination_draft,
            pending_routes: routes,
        });
    }
    candidates
}

fn pending_by_credential(
    db: &Database,
    snapshot: &RoutingSnapshot,
    facts: &[CredentialFact],
) -> Result<BTreeMap<String, Vec<PendingApprovedRoute>>, ExecutionError> {
    let zen = db
        .zen_free_model_catalog()
        .map_err(|_| unavailable("validation contracts could not be read"))?
        .unwrap_or_default();
    let custom = db
        .list_custom_account_runtimes()
        .map_err(|_| unavailable("validation contracts could not be read"))?;
    let persisted = db
        .load_persisted_contracts()
        .map_err(|_| unavailable("validation contracts could not be read"))?;
    let contracts = build_effective_contracts(&zen, &custom, persisted);
    let mut pending = BTreeMap::new();
    for fact in facts {
        let Some(destination) = snapshot
            .projection
            .destinations
            .iter()
            .find(|destination| destination.id == fact.destination_id)
        else {
            continue;
        };
        let scope = match &destination.legacy {
            LegacyDestinationRef::Builtin(id) => Some(ContractScope::provider(id.clone())),
            LegacyDestinationRef::CustomAccount(id) => {
                Some(ContractScope::custom_endpoint(id.clone()))
            }
            LegacyDestinationRef::Dynamic(_) | LegacyDestinationRef::PlatformParent(_) => None,
        };
        let Some(scope) = scope else {
            continue;
        };
        let Some(contract) = contracts.scope(&scope) else {
            continue;
        };
        let provider_id = snapshot
            .credentials
            .iter()
            .find(|credential| credential.credential_id == fact.credential_id)
            .map(|credential| credential.provider_id.as_str());
        let mut routes = Vec::new();
        for model in &destination.catalog {
            let evidence = contract
                .model(&model.upstream_model)
                .or_else(|| contract.model(&model.public_model));
            let Some(evidence) = evidence else {
                continue;
            };
            let client = client_protocols(destination, model);
            for row in evidence.protocols.values() {
                if client.iter().any(|protocol| *protocol == row.protocol) {
                    continue;
                }
                if !row.available
                    || row.r#override == ProtocolOverrideState::ForceOff
                    || !row.source.confers_support()
                {
                    continue;
                }
                if let Some(provider_id) = provider_id
                    && let Some(plan) = builtin_provider(provider_id)
                    && !plan.upstream_protocols.contains(&row.protocol)
                {
                    continue;
                }
                routes.push(PendingApprovedRoute {
                    public_model: model.public_model.clone(),
                    upstream_model: model.upstream_model.clone(),
                    protocol: row.protocol.as_str().to_string(),
                });
            }
        }
        routes.sort_by(|left, right| {
            left.public_model
                .cmp(&right.public_model)
                .then(left.upstream_model.cmp(&right.upstream_model))
                .then(left.protocol.cmp(&right.protocol))
        });
        routes.dedup();
        if !routes.is_empty() {
            pending.insert(fact.credential_id.clone(), routes);
        }
    }
    Ok(pending)
}

fn scope_emits(scope_json: &str, public_model: &str) -> bool {
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

fn client_protocols(
    destination: &ocg_domain::destination::Destination,
    model: &ocg_domain::destination::CatalogModel,
) -> Vec<UpstreamProtocolKind> {
    if model.upstream_override.is_some() || model.protocols.is_empty() {
        ocg_domain::destination::http_model_protocols(destination, model)
    } else {
        model.protocols.clone()
    }
}

fn native_route_sets(refs: &[OAuthFileRef], facts: &[CredentialFact]) -> Vec<CredentialRouteSet> {
    refs.iter()
        .filter_map(|item| {
            let fact = facts
                .iter()
                .find(|fact| fact.credential_id == item.credential_id)?;
            let inclusion = native_inclusion(&validation_row(fact))?;
            let validation_only = matches!(inclusion, NativeInclusion::ValidationOnly);
            let mut models = item.models.clone();
            models.sort();
            models.dedup();
            let facts = crate::cpa_projection::NativeAuthorityFacts {
                raw_label: item.raw_provider_label.clone(),
                provider: super::native::provider_label(item.provider).to_string(),
                mode: item.native_mode.clone(),
                reported_base: item.reported_base.clone(),
            };
            let routes = models
                .into_iter()
                .filter(|model| scope_emits(&fact.scope_json, model))
                .flat_map(|model| {
                    super::native::routes_from_facts(&facts, &model)
                        .into_iter()
                        .filter(|sealed| {
                            !sealed.protocol.is_empty()
                                && !sealed.endpoint_id.is_empty()
                                && !sealed.origin.is_empty()
                                && !sealed.endpoint_fingerprint.is_empty()
                        })
                        .map(move |sealed| NormalizedRoute {
                            public_model: model.clone(),
                            upstream_model: model.clone(),
                            protocol: sealed.protocol,
                            endpoint_id: sealed.endpoint_id,
                            origin: sealed.origin,
                            endpoint_fingerprint: sealed.endpoint_fingerprint,
                            validation_only,
                            native_targets: sealed.native_targets,
                        })
                })
                .collect::<Vec<_>>();
            if routes.is_empty() {
                return None;
            }
            let material_fingerprint = if item.material_revision.trim().is_empty() {
                "no-material".to_string()
            } else {
                item.material_revision.clone()
            };
            let fingerprint = credential_route_fingerprint(
                &item.auth_id,
                &item.credential_id,
                item.credential_version,
                &fact.binding_id,
                &material_fingerprint,
                item.routing_rank,
                &routes,
            );
            Some(CredentialRouteSet {
                auth_id: item.auth_id.clone(),
                credential_id: item.credential_id.clone(),
                credential_version: item.credential_version,
                binding_id: fact.binding_id.clone(),
                material_fingerprint,
                routing_rank: item.routing_rank,
                routes,
                fingerprint,
            })
        })
        .collect()
}

/// Owned envelope fields already rendered into config.yaml.
///
/// Credential material, model routes, native annotations, and the projection
/// revision stay on the proven document.
pub(super) struct OwnedEnvelope {
    pub process_generation: u64,
    pub listen_port: u16,
    pub auth_dir: String,
    pub policy_url: String,
    pub policy_token: String,
    pub policy_origin: String,
    pub ready_key: String,
    pub hop_secret: String,
}

pub(super) struct ReboundYaml {
    pub yaml: String,
    pub wire_digest: String,
    pub generation: u64,
    pub revision: u64,
}

impl std::fmt::Debug for ReboundYaml {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReboundYaml")
            .field("yaml", &"[redacted]")
            .field("wire_digest", &self.wire_digest)
            .field("generation", &self.generation)
            .field("revision", &self.revision)
            .finish()
    }
}

/// Prove `yaml` is the expected projection, then return it unchanged when the
/// owned envelope already matches. Otherwise rewrite only those envelope fields.
pub(super) fn rebind_proven_envelope(
    yaml: &str,
    expected_generation: u64,
    expected_revision: u64,
    expected_digest: &str,
    envelope: &OwnedEnvelope,
) -> Result<ReboundYaml, ExecutionError> {
    let (generation, revision, digest) = super::yaml_identity(yaml)?;
    if generation != expected_generation
        || revision != expected_revision
        || !digest.eq_ignore_ascii_case(expected_digest)
    {
        return Err(ExecutionError::Invalid(
            "CPA config projection identity does not match".into(),
        ));
    }
    if envelope_matches(yaml, envelope) {
        return Ok(ReboundYaml {
            yaml: yaml.to_string(),
            wire_digest: digest,
            generation,
            revision,
        });
    }
    let rebound = rewrite_owned_envelope(yaml, envelope)?;
    let wire_digest = crate::cpa_projection::wire_digest(&rebound);
    let (out_generation, out_revision, out_digest) = super::yaml_identity(&rebound)?;
    if out_generation != envelope.process_generation
        || out_revision != revision
        || out_digest != wire_digest
    {
        return Err(ExecutionError::Invalid(
            "rebound CPA config identity does not match".into(),
        ));
    }
    Ok(ReboundYaml {
        yaml: rebound,
        wire_digest,
        generation: out_generation,
        revision: out_revision,
    })
}

pub(super) fn envelope_matches(yaml: &str, envelope: &OwnedEnvelope) -> bool {
    let Ok(value) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(yaml) else {
        return false;
    };
    let Some(root) = as_map(&value) else {
        return false;
    };
    let Some(ocg) = root.get(&yaml_key("ocg")).and_then(as_map) else {
        return false;
    };
    let Some(policy) = ocg.get(&yaml_key("policy")).and_then(as_map) else {
        return false;
    };
    yaml_u16(root.get(&yaml_key("port"))) == Some(envelope.listen_port)
        && yaml_string(root.get(&yaml_key("auth-dir"))).as_deref()
            == Some(envelope.auth_dir.as_str())
        && one_hop(root.get(&yaml_key("api-keys"))).as_deref() == Some(envelope.hop_secret.as_str())
        && yaml_u64(ocg.get(&yaml_key("process-generation"))) == Some(envelope.process_generation)
        && yaml_string(ocg.get(&yaml_key("ready-key"))).as_deref()
            == Some(envelope.ready_key.as_str())
        && yaml_string(policy.get(&yaml_key("url"))).as_deref()
            == Some(envelope.policy_url.as_str())
        && yaml_string(policy.get(&yaml_key("token"))).as_deref()
            == Some(envelope.policy_token.as_str())
        && yaml_string(policy.get(&yaml_key("origin"))).as_deref()
            == Some(envelope.policy_origin.as_str())
}

fn unreadable_config() -> ExecutionError {
    ExecutionError::Invalid("CPA config could not be read".into())
}

fn rewrite_owned_envelope(yaml: &str, envelope: &OwnedEnvelope) -> Result<String, ExecutionError> {
    let mut value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(yaml).map_err(|_| unreadable_config())?;
    let root = match &mut value {
        serde_yaml_ng::Value::Mapping(mapping) => mapping,
        _ => return Err(unreadable_config()),
    };
    let keys = root
        .get_mut(&yaml_key("api-keys"))
        .ok_or_else(unreadable_config)?;
    let sequence = match keys {
        serde_yaml_ng::Value::Sequence(items) if items.len() == 1 => items,
        _ => {
            return Err(ExecutionError::Invalid(
                "CPA config hop secret is not one api key".into(),
            ));
        }
    };
    sequence[0] = serde_yaml_ng::Value::String(envelope.hop_secret.clone());
    root.insert(
        yaml_key("port"),
        serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(u64::from(envelope.listen_port))),
    );
    root.insert(
        yaml_key("auth-dir"),
        serde_yaml_ng::Value::String(envelope.auth_dir.clone()),
    );
    let ocg = match root.get_mut(&yaml_key("ocg")) {
        Some(serde_yaml_ng::Value::Mapping(mapping)) => mapping,
        _ => return Err(unreadable_config()),
    };
    ocg.insert(
        yaml_key("process-generation"),
        serde_yaml_ng::Value::String(envelope.process_generation.to_string()),
    );
    ocg.insert(
        yaml_key("ready-key"),
        serde_yaml_ng::Value::String(envelope.ready_key.clone()),
    );
    let policy = match ocg.get_mut(&yaml_key("policy")) {
        Some(serde_yaml_ng::Value::Mapping(mapping)) => mapping,
        _ => return Err(unreadable_config()),
    };
    policy.insert(
        yaml_key("url"),
        serde_yaml_ng::Value::String(envelope.policy_url.clone()),
    );
    policy.insert(
        yaml_key("token"),
        serde_yaml_ng::Value::String(envelope.policy_token.clone()),
    );
    policy.insert(
        yaml_key("origin"),
        serde_yaml_ng::Value::String(envelope.policy_origin.clone()),
    );
    let mut rendered = serde_yaml_ng::to_string(&value)
        .map_err(|_| ExecutionError::Invalid("CPA config could not be written".into()))?;
    if let Some(rest) = rendered.strip_prefix("---\n") {
        rendered = rest.to_string();
    }
    if let Some(rest) = rendered.strip_prefix("---\r\n") {
        rendered = rest.to_string();
    }
    Ok(rendered)
}

fn yaml_key(name: &str) -> serde_yaml_ng::Value {
    serde_yaml_ng::Value::String(name.to_string())
}

fn as_map(value: &serde_yaml_ng::Value) -> Option<&serde_yaml_ng::Mapping> {
    match value {
        serde_yaml_ng::Value::Mapping(mapping) => Some(mapping),
        _ => None,
    }
}

fn yaml_string(value: Option<&serde_yaml_ng::Value>) -> Option<String> {
    value
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
}

fn yaml_u64(value: Option<&serde_yaml_ng::Value>) -> Option<u64> {
    match value? {
        serde_yaml_ng::Value::String(text) => text.parse().ok(),
        serde_yaml_ng::Value::Number(number) => number.as_u64(),
        _ => None,
    }
}

fn yaml_u16(value: Option<&serde_yaml_ng::Value>) -> Option<u16> {
    u16::try_from(yaml_u64(value)?).ok()
}

fn one_hop(value: Option<&serde_yaml_ng::Value>) -> Option<String> {
    let items = value?.as_sequence()?;
    if items.len() != 1 {
        return None;
    }
    items
        .first()
        .and_then(serde_yaml_ng::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests;
