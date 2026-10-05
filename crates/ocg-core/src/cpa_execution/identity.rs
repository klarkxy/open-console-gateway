//! Live credential fence. OAuth token bytes are never read here.
//!
//! Auth ids and material fingerprints come from `cpa_projection`. Each attempt
//! UUID keeps the binding captured at its own admission.

#[cfg(test)]
use super::ExecutionError;
use super::store::{self, AuthStamp, Record};
use crate::cpa_observation::{
    AdmittedAttemptContext, AttemptErrorCode, AttemptOutcome, AttemptRecord, CapturedClientKey,
    ObservationError, StrictAttemptResult, record_result_on,
};
use crate::cpa_policy::{
    AppliedProjection, AttemptIdentity, CurrentCredential, CurrentFacts, DeclaredScope,
    DeclaredSubject, ErrorCode, Outcome, PolicyFacts, PolicyFault, PoolMembership, Reason,
    ResultBody, SendKind,
};
use crate::cpa_projection::{
    CredentialRouteSet, MAX_NATIVE_TARGETS, NativeAuthorityFacts, NativeDispatchTarget,
    NativeEndpointPin, NativeInclusion, NativeSourceOperation, NativeTargetOutcome,
    NormalizedRoute, ValidationCandidateKind, ValidationRow, accept_stored_native_targets, auth_id,
    canonical_native_url, classify_validation_with_auth, endpoint_fingerprint,
    endpoint_pin_capability_listed, material_fingerprint, native_inclusion, native_route_targets,
    native_targets_for, registration_is_complete, targets_for_applied_route,
};
use chrono::{DateTime, Utc};
use ocg_domain::catalog::UpstreamProtocolKind;
use ocg_domain::connection::{
    ConnectionId, EndpointOperation, LegacyConnectionKind, connection_id_for_legacy,
};
use ocg_domain::credential::{
    ModelScope, RouteSpec, assigned_endpoints_for_routes, origins_equivalent,
};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, Capabilities, CatalogModel, Destination, LegacyDestinationRef,
    ModelResolution, Plan, http_configured_routes, http_model_protocols, http_model_route,
};
use ocg_domain::ids::{
    COMMAND_CODE_PROVIDER_ID, KIMI_PROVIDER_ID, MINIMAX_PROVIDER_ID, OLLAMA_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, OPENCODE_ZEN_FREE_PROVIDER_ID, model_ids_match,
};
use ocg_domain::protocol::{ApiFormat, command_code_upstream_path};
use ocg_infra::crypto::KeyCipher;
use parking_lot::Mutex;
use rusqlite::{OptionalExtension, Transaction};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use zeroize::Zeroize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FenceMode {
    Ready,
    Applied,
}

#[derive(Clone, Debug)]
pub(super) struct CapturedAttempt {
    pub request_id: Uuid,
    pub attempt_id: Uuid,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub public_model: String,
    pub upstream_model: String,
    pub registration_epoch: u64,
    pub material_revision: String,
    pub kind: SendKind,
    /// Private admit pair. Results leave both empty.
    pub callable_protocol: String,
    pub generation_kind: String,
}

const CALLABLE_PROTOCOLS: [&str; 3] = ["chat_completions", "responses", "messages"];
const GENERATION_KINDS: [&str; 7] = [
    "execute",
    "refresh-resend",
    "stream",
    "stream-refresh",
    "stream-bootstrap",
    "internal",
    "count-tokens",
];

/// Full identity and allow-time pins frozen when an attempt is actually allowed.
#[derive(Clone, Debug)]
pub(super) struct PinnedAttempt {
    pub context: AdmittedAttemptContext,
    pub endpoint_pins: Option<Vec<NativeEndpointPin>>,
    /// Meter snapshot from the allow. A result never recaptures the live meter.
    pub credit: Option<crate::billing::CreditAttempt>,
}

#[derive(Clone, Debug)]
pub(super) struct ReadyIdentity {
    pub generation: u64,
    pub revision: u64,
    pub digest: String,
}

pub(super) struct LiveFacts {
    pub mode: FenceMode,
    pub attempt: Option<CapturedAttempt>,
    pub ready: Option<ReadyIdentity>,
    pub deadline: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub cipher: Arc<dyn KeyCipher + Send + Sync>,
    pub pin: Arc<Mutex<HashMap<Uuid, PinnedAttempt>>>,
    /// Admit revalidates caller intent. Results leave this false.
    pub gate_intent: bool,
    pub authorization: Option<super::CorrelationAuthorization>,
    pub requested_model: String,
    /// Public `ocg-` trace captured with the correlation. Results do not reread it.
    pub client_trace_id: Option<String>,
    /// Set when the log statement fails. The policy decision still commits.
    pub observation_fault: Arc<Mutex<Option<&'static str>>>,
    /// Pending or Absent OAuth on the attempt credential cannot mutate restrictions.
    pub restriction_authority: bool,
}

impl CurrentFacts for LiveFacts {
    fn revalidate(&mut self, tx: &Transaction<'_>) -> Result<PolicyFacts, PolicyFault> {
        let mut record = store::load_tx(tx).map_err(|_| PolicyFault::Unavailable)?;
        let attempt_credential = self
            .attempt
            .as_ref()
            .map(|captured| captured.credential_id.as_str())
            .unwrap_or("");
        self.restriction_authority = !oauth_presence_blocks(&record, attempt_credential);
        let mode = resolved_mode(self.mode, self.ready.as_ref(), &record);
        // Admit only, before current material is read. Results leave gate_intent false.
        if self.gate_intent {
            if let Some(captured) = &self.attempt {
                super::native::advance_refresh_on_tx(tx, &mut record, captured)?;
            }
        }
        let applied = fence_projection(&record, mode)?;
        let (map, routes) = authority(&record, mode);
        let mut attempt = match &self.attempt {
            None => None,
            Some(captured) => {
                match build_attempt(
                    tx,
                    &record,
                    map,
                    captured,
                    &self.pin,
                    self.cipher.as_ref(),
                    self.gate_intent,
                )? {
                    PinBuild::Identity(identity) => Some(identity),
                    PinBuild::Capacity => None,
                }
            }
        };
        let mut current = match &attempt {
            None => None,
            Some(attempt) => Some(build_current(
                tx,
                &record,
                map,
                attempt,
                self.cipher.as_ref(),
            )?),
        };
        let (caller_stop, validated_pin) = if self.gate_intent {
            let captured = self.attempt.as_ref();
            apply_caller_intent(
                tx,
                &record,
                &mut attempt,
                &mut current,
                map,
                routes,
                &self.authorization,
                &self.requested_model,
                captured
                    .map(|item| item.callable_protocol.as_str())
                    .unwrap_or(""),
                captured
                    .map(|item| item.generation_kind.as_str())
                    .unwrap_or(""),
                self.cipher.as_ref(),
            )?
        } else {
            (None, false)
        };
        Ok(PolicyFacts {
            applied,
            attempt,
            current,
            deadline_at: self.deadline,
            now: self.now,
            caller_stop,
            validated_pin,
        })
    }

    fn freeze_admitted_attempt(
        &mut self,
        tx: &Transaction<'_>,
        identity: &AttemptIdentity,
        allowed_at: DateTime<Utc>,
    ) -> Result<Option<Vec<NativeEndpointPin>>, PolicyFault> {
        {
            let pins = self.pin.lock();
            if let Some(existing) = pins.get(&identity.attempt_id) {
                return Ok(existing.endpoint_pins.clone());
            }
            if pins.len() >= super::ATTEMPT_PIN_CAP {
                return Ok(None);
            }
        }
        let endpoint_pins = granted_endpoint_pins(self, tx, identity)?;
        let mut pins = self.pin.lock();
        if let Some(existing) = pins.get(&identity.attempt_id) {
            return Ok(existing.endpoint_pins.clone());
        }
        if pins.len() >= super::ATTEMPT_PIN_CAP {
            return Ok(None);
        }
        let ordinal = u32::try_from(pins.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        if ordinal == 0 {
            return Ok(None);
        }
        let client_key = allow_time_client_key(tx, &self.authorization, identity.kind)?;
        let credit = admitted_credit_snapshot(tx, identity, allowed_at);
        pins.insert(
            identity.attempt_id,
            PinnedAttempt {
                context: AdmittedAttemptContext {
                    request_id: identity.request_id,
                    client_trace_id: self.client_trace_id.clone(),
                    attempt_id: identity.attempt_id,
                    ordinal,
                    started_at: allowed_at,
                    provider_id: identity.provider_id.clone(),
                    credential_id: identity.credential_id.clone(),
                    credential_version: identity.credential_version,
                    binding_id: identity.binding_id.clone(),
                    auth_id: identity.auth_id.clone(),
                    material_revision: identity.material_revision.clone(),
                    registration_epoch: identity.registration_epoch,
                    public_model: identity.public_model.clone(),
                    upstream_model: identity.upstream_model.clone(),
                    kind: identity.kind,
                    client_key,
                },
                endpoint_pins: endpoint_pins.clone(),
                credit,
            },
        );
        Ok(endpoint_pins)
    }

    fn frozen_endpoint_pins(&self, attempt_id: Uuid) -> Option<Option<Vec<NativeEndpointPin>>> {
        self.pin
            .lock()
            .get(&attempt_id)
            .map(|pinned| pinned.endpoint_pins.clone())
    }

    fn retains_quota_restriction_authority(&self) -> bool {
        self.restriction_authority
    }

    fn record_admitted_result(
        &mut self,
        tx: &Transaction<'_>,
        _decision: &crate::cpa_policy::Decision,
        identity: &AttemptIdentity,
        result: &ResultBody,
        raw_body: &str,
    ) -> Result<(), PolicyFault> {
        let (admitted, credit) = {
            let pins = self.pin.lock();
            let Some(pinned) = pins.get(&identity.attempt_id) else {
                return Ok(());
            };
            (pinned.context.clone(), pinned.credit.clone())
        };
        match record_result_on(tx, &admitted, &strict_result(result), raw_body) {
            Ok(AttemptRecord::Inserted(log_id)) => {
                if let Some(attempt) = credit.as_ref() {
                    settle_pinned_credit(tx, attempt, log_id)?;
                }
                Ok(())
            }
            Ok(AttemptRecord::Existing(_))
            | Err(
                ObservationError::Identity
                | ObservationError::Mismatch
                | ObservationError::Usage
                | ObservationError::Conflict,
            ) => Ok(()),
            Err(ObservationError::Store) => {
                *self.observation_fault.lock() = Some("event=cpa_observation_store_failed");
                Ok(())
            }
        }
    }
}

/// Settle the allow-time snapshot. A missing receipt attachment or a balance
/// write failure fails this transaction so the debit cannot commit alone.
/// Pricing does not decide the attempt. No meter was captured when none matched.
fn settle_pinned_credit(
    tx: &Transaction<'_>,
    attempt: &crate::billing::CreditAttempt,
    log_id: i64,
) -> Result<(), PolicyFault> {
    let (status, prompt, completion): (String, i64, i64) = tx
        .query_row(
            "SELECT status, prompt_tokens, completion_tokens FROM forward_logs WHERE id=?1",
            [log_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    if status != "success" {
        return Ok(());
    }
    crate::db::billing::attach_attempt_on(tx, log_id, attempt)
        .map_err(|_| PolicyFault::Unavailable)?;
    if !crate::db::billing::attached_pending_on(tx, log_id, attempt)
        .map_err(|_| PolicyFault::Unavailable)?
    {
        return Err(PolicyFault::Unavailable);
    }
    let tokens = ocg_domain::billing::BillingTokens::new(prompt.max(0), completion.max(0), 0, 0);
    crate::db::billing::settle_on(tx, log_id, attempt, tokens, "success", Utc::now())
        .map_err(|_| PolicyFault::Unavailable)
}

/// Optional allow-time meter. A read or configuration error leaves no snapshot
/// and does not refuse the attempt.
fn admitted_credit_snapshot(
    tx: &Transaction<'_>,
    identity: &AttemptIdentity,
    at: DateTime<Utc>,
) -> Option<crate::billing::CreditAttempt> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT c.legacy_account_id, d.base_url
             FROM credentials c
             JOIN destinations d ON d.id = c.destination_id
             WHERE c.id=?1",
            [identity.credential_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .ok()?;
    let Some((legacy, endpoint)) = row else {
        return None;
    };
    if legacy.is_empty() || endpoint.is_empty() {
        return None;
    }
    crate::db::billing::capture_on(tx, &legacy, &endpoint, &identity.upstream_model, at)
        .ok()
        .flatten()
        .filter(|attempt| {
            attempt.credential_id == identity.credential_id && attempt.account_id == legacy
        })
}

fn fence_projection(record: &Record, mode: FenceMode) -> Result<AppliedProjection, PolicyFault> {
    let (generation, revision, digest) = match mode {
        FenceMode::Ready => (
            record.child_generation,
            record.desired_revision,
            record.desired_digest.as_str(),
        ),
        FenceMode::Applied => (
            record.applied_generation,
            record.applied_revision,
            record.applied_digest.as_str(),
        ),
    };
    Ok(AppliedProjection {
        process_generation: generation,
        revision,
        digest: decode_digest(digest)?,
    })
}

fn resolved_mode(mode: FenceMode, ready: Option<&ReadyIdentity>, record: &Record) -> FenceMode {
    match (mode, ready) {
        (FenceMode::Ready, Some(ready))
            if ready.generation == record.applied_generation
                && ready.revision == record.applied_revision
                && ready.digest == record.applied_digest
                && !(ready.generation == record.child_generation
                    && ready.revision == record.desired_revision
                    && ready.digest == record.desired_digest) =>
        {
            FenceMode::Applied
        }
        (mode, _) => mode,
    }
}

fn authority<'a>(
    record: &'a Record,
    mode: FenceMode,
) -> (&'a [AuthStamp], &'a [CredentialRouteSet]) {
    match mode {
        FenceMode::Ready => (&record.desired_auth, &record.desired_routes),
        FenceMode::Applied => (&record.applied_auth, &record.applied_routes),
    }
}

fn decode_digest(value: &str) -> Result<[u8; 32], PolicyFault> {
    if value.len() != 64 {
        return Err(PolicyFault::Malformed);
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| PolicyFault::Malformed)?;
    }
    Ok(digest)
}

enum PinBuild {
    Identity(AttemptIdentity),
    Capacity,
}

fn build_attempt(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    captured: &CapturedAttempt,
    pin: &Mutex<HashMap<Uuid, PinnedAttempt>>,
    cipher: &dyn KeyCipher,
    admit_candidate: bool,
) -> Result<PinBuild, PolicyFault> {
    {
        let pins = pin.lock();
        if let Some(frozen) = pins.get(&captured.attempt_id) {
            return Ok(PinBuild::Identity(identity_from_admitted(&frozen.context)));
        }
        if admit_candidate && pins.len() >= super::ATTEMPT_PIN_CAP {
            return Ok(PinBuild::Capacity);
        }
    }
    let binding = map
        .iter()
        .find(|stamp| stamp.credential_id == captured.credential_id)
        .map(|stamp| stamp.binding_id.clone())
        .filter(|binding| !binding.is_empty())
        .unwrap_or_else(|| "unapplied".to_string());
    let _live = current_material(tx, record, &captured.credential_id, cipher)?;
    Ok(PinBuild::Identity(attempt_identity(captured, binding)))
}

fn identity_from_admitted(admitted: &AdmittedAttemptContext) -> AttemptIdentity {
    AttemptIdentity {
        request_id: admitted.request_id,
        attempt_id: admitted.attempt_id,
        auth_id: admitted.auth_id.clone(),
        credential_id: admitted.credential_id.clone(),
        credential_version: admitted.credential_version,
        provider_id: admitted.provider_id.clone(),
        public_model: admitted.public_model.clone(),
        upstream_model: admitted.upstream_model.clone(),
        binding_id: admitted.binding_id.clone(),
        material_revision: admitted.material_revision.clone(),
        registration_epoch: admitted.registration_epoch,
        kind: admitted.kind,
    }
}

fn allow_time_client_key(
    tx: &Transaction<'_>,
    authorization: &Option<super::CorrelationAuthorization>,
    kind: SendKind,
) -> Result<Option<CapturedClientKey>, PolicyFault> {
    if kind != SendKind::Accepted {
        return Ok(None);
    }
    let Some(super::CorrelationAuthorization::Client { key_id, .. }) = authorization else {
        return Ok(None);
    };
    if key_id.is_empty() {
        return Ok(None);
    }
    let name: Option<String> = tx
        .query_row(
            "SELECT name FROM access_keys WHERE id = ?1",
            [key_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(Some(CapturedClientKey {
        id: key_id.clone(),
        name,
    }))
}

fn strict_result(body: &ResultBody) -> StrictAttemptResult {
    StrictAttemptResult {
        sent: body.sent,
        status: body.status,
        body_complete: body.body_complete,
        stream_started: body.stream_started,
        outcome: match body.outcome {
            Outcome::Success => AttemptOutcome::Success,
            Outcome::ExplicitRejection => AttemptOutcome::ExplicitRejection,
            Outcome::Uncertain => AttemptOutcome::Uncertain,
            Outcome::Cancelled => AttemptOutcome::Cancelled,
            Outcome::Deadline => AttemptOutcome::Deadline,
            Outcome::LocalFailure => AttemptOutcome::LocalFailure,
        },
        error_code: match body.error_code {
            ErrorCode::Validation => AttemptErrorCode::Validation,
            ErrorCode::Transport => AttemptErrorCode::Transport,
            ErrorCode::ProviderRejected => AttemptErrorCode::ProviderRejected,
            ErrorCode::BodyLost => AttemptErrorCode::BodyLost,
            ErrorCode::StreamLost => AttemptErrorCode::StreamLost,
            ErrorCode::Cancelled => AttemptErrorCode::Cancelled,
            ErrorCode::Deadline => AttemptErrorCode::Deadline,
            ErrorCode::UsageObservation => AttemptErrorCode::UsageObservation,
            ErrorCode::Parser => AttemptErrorCode::Parser,
            ErrorCode::None => AttemptErrorCode::None,
            ErrorCode::Unknown => AttemptErrorCode::Unknown,
        },
        observation_id: body.observation.as_ref().map(|item| item.id.clone()),
        observed_at: body.observation.as_ref().map(|item| item.fetched_at),
    }
}

fn attempt_identity(captured: &CapturedAttempt, binding: String) -> AttemptIdentity {
    AttemptIdentity {
        request_id: captured.request_id,
        attempt_id: captured.attempt_id,
        auth_id: captured.auth_id.clone(),
        credential_id: captured.credential_id.clone(),
        credential_version: captured.credential_version,
        provider_id: captured.provider_id.clone(),
        public_model: captured.public_model.clone(),
        upstream_model: captured.upstream_model.clone(),
        binding_id: binding,
        material_revision: captured.material_revision.clone(),
        registration_epoch: captured.registration_epoch,
        kind: captured.kind,
    }
}

struct LiveRow {
    credential_id: String,
    legacy_account_id: String,
    destination_id: String,
    provider_id: String,
    binding_id: String,
    version: u64,
    key_cipher: String,
    enabled: bool,
    pending_setup: bool,
    scope_json: String,
}

fn build_current(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    attempt: &AttemptIdentity,
    cipher: &dyn KeyCipher,
) -> Result<CurrentCredential, PolicyFault> {
    let Some(row) = load_row(tx, &attempt.credential_id)? else {
        return Ok(deleted_current(attempt));
    };
    let stamp = map
        .iter()
        .find(|stamp| stamp.credential_id == row.credential_id);
    let rebound = match stamp {
        Some(stamp) => stamp.binding_id != row.binding_id,
        None => true,
    };
    let material = current_material(tx, record, &row.credential_id, cipher)?;
    let (credential_scopes, pool_scopes, memberships) =
        scopes_for(tx, &row).map_err(|_| PolicyFault::Unavailable)?;
    let mut scopes = credential_scopes;
    scopes.extend(pool_scopes);
    let registration_epoch = adopted_registration_epoch(
        map,
        &row.credential_id,
        row.version,
        &row.binding_id,
        adopt_stamp_for(
            tx,
            record,
            &row.credential_id,
            &row.destination_id,
            !row.key_cipher.is_empty(),
        )?,
        &material,
    );
    Ok(CurrentCredential {
        credential_id: row.credential_id,
        credential_version: row.version,
        provider_id: row.provider_id,
        binding_id: row.binding_id,
        material_revision: material.revision,
        registration_epoch,
        auth_id: material.auth_id,
        memberships,
        scopes,
        granted: granted(tx, &attempt.credential_id)?
            && !oauth_presence_blocks(record, &attempt.credential_id),
        enabled: row.enabled,
        deleted: false,
        rebound,
    })
}

struct MaterialFact {
    revision: String,
    auth_id: String,
    epoch: u64,
}

fn current_material(
    tx: &Transaction<'_>,
    record: &Record,
    credential_id: &str,
    cipher: &dyn KeyCipher,
) -> Result<MaterialFact, PolicyFault> {
    let Some(row) = load_row(tx, credential_id)? else {
        return Ok(MaterialFact {
            revision: "deleted".into(),
            auth_id: "deleted".into(),
            epoch: 0,
        });
    };
    if row.key_cipher.is_empty() {
        if let Some(oauth) = record
            .oauth
            .iter()
            .find(|stamp| stamp.credential_id == credential_id)
        {
            let revision = if oauth.material_revision.is_empty() {
                "unreported".to_string()
            } else {
                oauth.material_revision.clone()
            };
            let auth = if oauth.auth_id.is_empty() {
                auth_id(credential_id, row.version, &row.binding_id, None)
            } else {
                oauth.auth_id.clone()
            };
            return Ok(MaterialFact {
                revision,
                auth_id: auth,
                epoch: oauth.registration_epoch,
            });
        }
        return Ok(MaterialFact {
            revision: "no-material".into(),
            auth_id: auth_id(credential_id, row.version, &row.binding_id, None),
            epoch: 0,
        });
    }
    match cipher.decrypt(&row.key_cipher) {
        Ok(mut plain) => {
            let revision = material_fingerprint(&plain);
            plain.zeroize();
            let auth = auth_id(credential_id, row.version, &row.binding_id, Some(&revision));
            Ok(MaterialFact {
                revision,
                auth_id: auth,
                epoch: 0,
            })
        }
        Err(_) => Ok(MaterialFact {
            revision: "decrypt-failed".into(),
            auth_id: auth_id(
                credential_id,
                row.version,
                &row.binding_id,
                Some("decrypt-failed"),
            ),
            epoch: 0,
        }),
    }
}

/// The host assigns a registration epoch when it registers an in-memory auth.
/// Keyed material and configured HTTP no-auth have no epoch of their own, so
/// one exact stamp supplies it. Native OAuth keeps the oauth stamp epoch.
fn adopted_registration_epoch(
    map: &[AuthStamp],
    credential_id: &str,
    version: u64,
    binding_id: &str,
    adopt_stamp: bool,
    material: &MaterialFact,
) -> u64 {
    if !adopt_stamp || binding_id.is_empty() {
        return material.epoch;
    }
    let mut matched = None;
    for stamp in map {
        if stamp.credential_id != credential_id
            || stamp.credential_version != version
            || stamp.binding_id != binding_id
            || stamp.auth_id != material.auth_id
            || stamp.material_revision != material.revision
        {
            continue;
        }
        if matched.is_some() {
            return material.epoch;
        }
        matched = Some(stamp.registration_epoch);
    }
    matched.unwrap_or(material.epoch)
}

/// Keyed credentials adopt the host stamp. Configured HTTP `AuthScheme::None`
/// and the registered Zen Free none singleton do too. Empty ciphertext on a
/// native OAuth row does not.
fn adopt_stamp_for(
    tx: &Transaction<'_>,
    record: &Record,
    credential_id: &str,
    destination_id: &str,
    key_present: bool,
) -> Result<bool, PolicyFault> {
    if key_present || !oauth_rows(record, credential_id).is_empty() {
        return Ok(key_present);
    }
    let Some(destination) = load_destination(tx, destination_id)? else {
        return Ok(false);
    };
    Ok(
        (destination.adapter == AdapterKind::Http && destination.auth_scheme == AuthScheme::None)
            || zen_free_none_singleton(&destination, credential_id),
    )
}

/// The built-in Zen Free account is the only keyless singleton that adopts a
/// host registration epoch. A bearer row with an empty secret does not.
fn zen_free_none_singleton(destination: &Destination, credential_id: &str) -> bool {
    let singleton = ocg_domain::credential::credential_id_for_legacy_account(
        ocg_domain::ids::ZEN_FREE_ACCOUNT_ID,
    );
    destination.adapter == AdapterKind::Zen
        && destination.auth_scheme == AuthScheme::None
        && destination.max_credentials == Some(1)
        && matches!(
            &destination.legacy,
            LegacyDestinationRef::Builtin(id) if id == OPENCODE_ZEN_FREE_PROVIDER_ID
        )
        && credential_id == singleton.as_str()
}

fn deleted_current(attempt: &AttemptIdentity) -> CurrentCredential {
    CurrentCredential {
        credential_id: attempt.credential_id.clone(),
        credential_version: attempt.credential_version,
        provider_id: attempt.provider_id.clone(),
        binding_id: attempt.binding_id.clone(),
        material_revision: attempt.material_revision.clone(),
        registration_epoch: 0,
        auth_id: attempt.auth_id.clone(),
        memberships: Vec::new(),
        scopes: vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: None,
        }],
        granted: false,
        enabled: false,
        deleted: true,
        rebound: false,
    }
}

fn load_row(tx: &Transaction<'_>, credential_id: &str) -> Result<Option<LiveRow>, PolicyFault> {
    let row = tx
        .query_row(
            "SELECT id, legacy_account_id, provider_id, destination_id, binding_id, binding_enabled,
                    credential_version, key_cipher, enabled, setup_step, scope_json
             FROM credentials WHERE id = ?1",
            [credential_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    let Some((
        id,
        legacy,
        provider,
        destination,
        binding,
        binding_enabled,
        version,
        key,
        enabled,
        step,
        scope,
    )) = row
    else {
        return Ok(None);
    };
    let pending_setup = match step.as_deref().map(str::trim) {
        None | Some("") | Some("ready") => false,
        Some(_) => true,
    };
    let version = version.unwrap_or(0);
    if version < 0 {
        return Err(PolicyFault::Malformed);
    }
    Ok(Some(LiveRow {
        credential_id: id,
        legacy_account_id: legacy,
        destination_id: destination.unwrap_or_default(),
        provider_id: provider.unwrap_or_default(),
        binding_id: binding.unwrap_or_default(),
        version: version as u64,
        key_cipher: key.unwrap_or_default(),
        enabled: enabled.unwrap_or(0) != 0 && binding_enabled.unwrap_or(1) != 0 && !pending_setup,
        pending_setup,
        scope_json: scope.unwrap_or_default(),
    }))
}

fn granted(tx: &Transaction<'_>, credential_id: &str) -> Result<bool, PolicyFault> {
    if !table_exists(tx, "credential_grants")? {
        return Ok(true);
    }
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM credential_grants WHERE credential_id = ?1",
            [credential_id],
            |row| row.get(0),
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(count > 0)
}

fn scopes_for(
    tx: &Transaction<'_>,
    row: &LiveRow,
) -> Result<(Vec<DeclaredScope>, Vec<DeclaredScope>, Vec<PoolMembership>), PolicyFault> {
    let scope = serde_json::from_str::<ModelScope>(&row.scope_json).ok();
    let Some(scope) = scope else {
        return Ok((Vec::new(), Vec::new(), Vec::new()));
    };
    let credential_scopes = match &scope {
        ModelScope::All => vec![DeclaredScope {
            subject: DeclaredSubject::Credential,
            public_model: None,
        }],
        ModelScope::Only { models } => models
            .iter()
            .map(|model| DeclaredScope {
                subject: DeclaredSubject::Credential,
                public_model: Some(model.clone()),
            })
            .collect(),
    };
    let pools = memberships(tx, &row.credential_id, &row.legacy_account_id)?;
    if pools.is_empty() {
        return Ok((credential_scopes, Vec::new(), Vec::new()));
    }
    let mut pool_scopes = Vec::new();
    let mut members = Vec::new();
    for pool_id in pools {
        match &scope {
            ModelScope::All => {
                members.push(PoolMembership {
                    pool_id: pool_id.clone(),
                    pool_version: 1,
                    public_models: Vec::new(),
                    all_models: true,
                });
                pool_scopes.push(DeclaredScope {
                    subject: DeclaredSubject::Pool {
                        pool_id,
                        pool_version: 1,
                    },
                    public_model: None,
                });
            }
            ModelScope::Only { models } => {
                members.push(PoolMembership {
                    pool_id: pool_id.clone(),
                    pool_version: 1,
                    public_models: models.clone(),
                    all_models: false,
                });
                for model in models {
                    pool_scopes.push(DeclaredScope {
                        subject: DeclaredSubject::Pool {
                            pool_id: pool_id.clone(),
                            pool_version: 1,
                        },
                        public_model: Some(model.clone()),
                    });
                }
            }
        }
    }
    Ok((credential_scopes, pool_scopes, members))
}

fn memberships(
    tx: &Transaction<'_>,
    credential_id: &str,
    legacy_account_id: &str,
) -> Result<Vec<String>, PolicyFault> {
    if !table_exists(tx, "quota_pool_members")? {
        return Ok(Vec::new());
    }
    let mut statement = tx
        .prepare(
            "SELECT pool_id FROM quota_pool_members
             WHERE account_id = ?1 OR account_id = ?2
             ORDER BY pool_id",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let rows = statement
        .query_map([credential_id, legacy_account_id], |row| row.get(0))
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut pools = Vec::new();
    for row in rows {
        pools.push(row.map_err(|_| PolicyFault::Unavailable)?);
    }
    Ok(pools)
}

fn table_exists(tx: &Transaction<'_>, name: &str) -> Result<bool, PolicyFault> {
    let found: Option<String> = tx
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(found.is_some())
}

/// Admit-only caller check. A failure stops the logical request. Results do not call this.
fn apply_caller_intent(
    tx: &Transaction<'_>,
    record: &Record,
    attempt: &mut Option<AttemptIdentity>,
    current: &mut Option<CurrentCredential>,
    map: &[AuthStamp],
    routes: &[CredentialRouteSet],
    authorization: &Option<super::CorrelationAuthorization>,
    requested_model: &str,
    callable_protocol: &str,
    generation_kind: &str,
    cipher: &dyn KeyCipher,
) -> Result<(Option<Reason>, bool), PolicyFault> {
    let Some(authorization) = authorization else {
        return Ok((Some(Reason::RequestFence), false));
    };
    let Some(attempt_identity) = attempt.as_mut() else {
        return Ok((Some(Reason::RequestFence), false));
    };
    if !CALLABLE_PROTOCOLS.contains(&callable_protocol)
        || !GENERATION_KINDS.contains(&generation_kind)
    {
        return Ok((Some(Reason::Malformed), false));
    }
    match authorization {
        super::CorrelationAuthorization::Client {
            key_id,
            captured_key_fingerprint,
        } => {
            if attempt_identity.kind != SendKind::Accepted {
                return Ok((Some(Reason::RequestFence), false));
            }
            if requested_model.is_empty() || attempt_identity.public_model != requested_model {
                return Ok((Some(Reason::ModelFence), false));
            }
            if !client_key_matches(tx, key_id, captured_key_fingerprint)? {
                return Ok((Some(Reason::Unauthorized), false));
            }
            let Some(live) = current.as_ref() else {
                return Ok((None, false));
            };
            if live.deleted {
                return Ok((None, false));
            }
            let Some(row) = load_admission(tx, &live.credential_id)? else {
                return Ok((None, false));
            };
            if row.pending_setup {
                return Ok((Some(Reason::Unauthorized), false));
            }
            if oauth_presence_blocks(record, &live.credential_id) {
                return Ok((Some(Reason::IdentityFence), false));
            }
            match native_inclusion(&validation_of(&row)) {
                Some(NativeInclusion::Client) => {}
                _ if !row.credential_enabled || !row.binding_enabled => {
                    return Ok((None, false));
                }
                _ => return Ok((Some(Reason::Unauthorized), false)),
            }
            Ok((
                finish_confirm(confirm_client(
                    tx,
                    record,
                    map,
                    routes,
                    &row,
                    live,
                    attempt_identity,
                    requested_model,
                    callable_protocol,
                    cipher,
                )?),
                false,
            ))
        }
        super::CorrelationAuthorization::Validated {
            credential_id,
            credential_version,
            requested_protocol,
        } => {
            if attempt_identity.kind != SendKind::Validated {
                return Ok((Some(Reason::RequestFence), false));
            }
            if requested_model.is_empty() || attempt_identity.public_model != requested_model {
                return Ok((Some(Reason::ModelFence), false));
            }
            if attempt_identity.credential_id != *credential_id
                || attempt_identity.credential_version != *credential_version
            {
                return Ok((Some(Reason::IdentityFence), false));
            }
            let Some(live) = current.as_mut() else {
                return Ok((Some(Reason::IdentityFence), false));
            };
            if live.deleted
                || live.credential_id != *credential_id
                || live.credential_version != *credential_version
            {
                return Ok((Some(Reason::IdentityFence), false));
            }
            let Some(row) = load_admission(tx, credential_id)? else {
                return Ok((Some(Reason::IdentityFence), false));
            };
            if row.version != *credential_version {
                return Ok((Some(Reason::IdentityFence), false));
            }
            if oauth_presence_blocks(record, &row.credential_id) {
                return Ok((Some(Reason::IdentityFence), true));
            }
            let satisfied = authentication_satisfied(tx, record, &row, cipher)?;
            let Some(kind) = classify_validation_with_auth(&validation_of(&row), satisfied) else {
                let stamped = map.iter().any(|stamp| {
                    stamp.credential_id == row.credential_id
                        && stamp.credential_version == row.version
                });
                let reason = if stamped {
                    Reason::Disabled
                } else {
                    Reason::IdentityFence
                };
                return Ok((Some(reason), true));
            };
            let confirmed = confirm_validated(
                tx,
                record,
                map,
                routes,
                &row,
                live,
                requested_model,
                requested_protocol,
                callable_protocol,
                kind,
                cipher,
            )?;
            if matches!(confirmed, Confirm::Ready)
                && kind == ValidationCandidateKind::ManagedKeyVerification
            {
                live.enabled = true;
            }
            Ok((finish_confirm(confirmed), true))
        }
    }
}

fn client_key_matches(
    tx: &Transaction<'_>,
    key_id: &str,
    captured_key_fingerprint: &str,
) -> Result<bool, PolicyFault> {
    if key_id.is_empty() || !super::lowercase_hex_64(captured_key_fingerprint) {
        return Ok(false);
    }
    let row = tx
        .query_row(
            "SELECT key, enabled, deleted_at FROM access_keys WHERE id = ?1",
            [key_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    let Some((mut key, enabled, deleted_at)) = row else {
        return Ok(false);
    };
    let fingerprint = crate::cpa_runtime::fingerprint_key(&key);
    key.zeroize();
    Ok(enabled != 0
        && deleted_at.is_none()
        && !fingerprint.is_empty()
        && fingerprint == captured_key_fingerprint)
}

enum Confirm {
    Ready,
    Rebound,
    Stop(Reason),
}

fn finish_confirm(confirm: Confirm) -> Option<Reason> {
    match confirm {
        Confirm::Ready | Confirm::Rebound => None,
        Confirm::Stop(reason) => Some(reason),
    }
}

struct Admission {
    credential_id: String,
    provider_id: String,
    destination_id: String,
    binding_id: String,
    version: u64,
    credential_enabled: bool,
    binding_enabled: bool,
    setup_step: String,
    account_type: String,
    material_present: bool,
    destination_enabled: bool,
    destination_draft: bool,
    connection_id: String,
    scope_json: String,
    pending_setup: bool,
}

struct Located<'a> {
    stamp: &'a AuthStamp,
    set: &'a CredentialRouteSet,
}

enum LocateMiss {
    Identity,
    Rebound,
    Routes,
}

fn confirm_client(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    routes: &[CredentialRouteSet],
    row: &Admission,
    live: &CurrentCredential,
    attempt: &AttemptIdentity,
    requested_model: &str,
    _callable_protocol: &str,
    _cipher: &dyn KeyCipher,
) -> Result<Confirm, PolicyFault> {
    let located = match locate(map, routes, row, live)? {
        Ok(located) => located,
        Err(miss) => return Ok(miss_confirm(miss)),
    };
    let selected: Vec<&NormalizedRoute> = located
        .set
        .routes
        .iter()
        .filter(|route| {
            model_ids_match(&route.public_model, requested_model) && !route.validation_only
        })
        .collect();
    // Callable protocol is the client format. The owned executor translates it
    // onto these routes; each route protocol remains the upstream format.
    if selected.is_empty() {
        return Ok(Confirm::Stop(Reason::ModelFence));
    }
    for route in selected {
        if !model_ids_match(&route.upstream_model, &attempt.upstream_model) {
            return Ok(Confirm::Stop(Reason::ModelFence));
        }
        if let Some(reason) = confirm_one(tx, record, map, row, route, requested_model)? {
            return Ok(Confirm::Stop(reason));
        }
    }
    Ok(Confirm::Ready)
}

fn confirm_validated(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    routes: &[CredentialRouteSet],
    row: &Admission,
    live: &CurrentCredential,
    requested_model: &str,
    requested_protocol: &str,
    callable_protocol: &str,
    kind: ValidationCandidateKind,
    _cipher: &dyn KeyCipher,
) -> Result<Confirm, PolicyFault> {
    let located = match locate(map, routes, row, live)? {
        Ok(located) => located,
        Err(miss) => return Ok(miss_confirm(miss)),
    };
    if requested_protocol != callable_protocol {
        return Ok(Confirm::Stop(Reason::ModelFence));
    }
    let Some(route) = one_protocol(located.set, requested_model, requested_protocol) else {
        return Ok(Confirm::Stop(Reason::ModelFence));
    };
    if !validated_route_allowed(kind, row.destination_draft, route.validation_only) {
        return Ok(Confirm::Stop(Reason::ModelFence));
    }
    if let Some(reason) = confirm_one(tx, record, map, row, route, requested_model)? {
        return Ok(Confirm::Stop(reason));
    }
    Ok(Confirm::Ready)
}

fn miss_confirm(miss: LocateMiss) -> Confirm {
    match miss {
        LocateMiss::Identity => Confirm::Stop(Reason::IdentityFence),
        LocateMiss::Rebound => Confirm::Rebound,
        LocateMiss::Routes => Confirm::Stop(Reason::ModelFence),
    }
}

fn validated_route_allowed(
    kind: ValidationCandidateKind,
    destination_draft: bool,
    validation_only: bool,
) -> bool {
    match kind {
        ValidationCandidateKind::ManagedKeyVerification => validation_only,
        ValidationCandidateKind::CompletePending if destination_draft => validation_only,
        ValidationCandidateKind::CompletePending => true,
    }
}

fn one_protocol<'a>(
    set: &'a CredentialRouteSet,
    public_model: &str,
    protocol: &str,
) -> Option<&'a NormalizedRoute> {
    let mut matched = set.routes.iter().filter(|route| {
        model_ids_match(&route.public_model, public_model) && route.protocol == protocol
    });
    let route = matched.next()?;
    matched.next().is_none().then_some(route)
}

fn locate<'a>(
    map: &'a [AuthStamp],
    routes: &'a [CredentialRouteSet],
    row: &Admission,
    live: &CurrentCredential,
) -> Result<Result<Located<'a>, LocateMiss>, PolicyFault> {
    let same_id: Vec<&AuthStamp> = map
        .iter()
        .filter(|stamp| stamp.credential_id == row.credential_id)
        .collect();
    if same_id.is_empty() {
        return Ok(Err(LocateMiss::Identity));
    }
    let versioned: Vec<&AuthStamp> = same_id
        .into_iter()
        .filter(|stamp| stamp.credential_version == row.version)
        .collect();
    if versioned.is_empty() {
        return Ok(Err(LocateMiss::Identity));
    }
    let bound: Vec<&AuthStamp> = versioned
        .into_iter()
        .filter(|stamp| !stamp.binding_id.is_empty() && stamp.binding_id == row.binding_id)
        .collect();
    if bound.len() != 1 {
        return Ok(Err(if bound.is_empty() {
            LocateMiss::Rebound
        } else {
            LocateMiss::Identity
        }));
    }
    let stamp = bound[0];
    if stamp.provider_id != live.provider_id
        || stamp.provider_id != row.provider_id
        || stamp.auth_id != live.auth_id
        || stamp.auth_id.is_empty()
        || stamp.registration_epoch != live.registration_epoch
        || stamp.credential_id != live.credential_id
        || stamp.credential_version != live.credential_version
        || stamp.binding_id != live.binding_id
    {
        return Ok(Err(LocateMiss::Identity));
    }
    let sets: Vec<&CredentialRouteSet> = routes
        .iter()
        .filter(|set| {
            set.auth_id == stamp.auth_id
                && set.credential_id == stamp.credential_id
                && set.credential_version == stamp.credential_version
                && set.binding_id == stamp.binding_id
        })
        .collect();
    if sets.len() != 1 || sets[0].routes.is_empty() {
        return Ok(Err(LocateMiss::Routes));
    }
    Ok(Ok(Located {
        stamp,
        set: sets[0],
    }))
}

fn confirm_one(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    row: &Admission,
    route: &NormalizedRoute,
    requested_model: &str,
) -> Result<Option<Reason>, PolicyFault> {
    if route.protocol.is_empty()
        || route.endpoint_id.is_empty()
        || route.origin.is_empty()
        || route.endpoint_fingerprint.is_empty()
        || route.upstream_model.is_empty()
        || !model_ids_match(&route.public_model, requested_model)
    {
        return Ok(Some(Reason::ModelFence));
    }
    if !scope_allows(&row.scope_json, requested_model) {
        return Ok(Some(Reason::ModelFence));
    }
    let Some(protocol) = protocol_kind(&route.protocol) else {
        return Ok(Some(Reason::ModelFence));
    };
    let destination = match load_destination(tx, &row.destination_id)? {
        Some(destination) => destination,
        None => return Ok(Some(Reason::ModelFence)),
    };
    if !destination.enabled || destination.id != row.destination_id {
        return Ok(Some(Reason::ModelFence));
    }
    let Some(model) = catalog_row(&destination.catalog, &route.upstream_model) else {
        return Ok(Some(Reason::ModelFence));
    };
    if !name_resolves(&row.provider_id, model, requested_model)
        || !protocol_open(&destination, model, protocol)
    {
        return Ok(Some(Reason::ModelFence));
    }
    if destination.adapter == AdapterKind::Cpa {
        return confirm_native_face(tx, record, map, row, route);
    }
    Ok(
        match http_face_block(tx, row, &destination, model, protocol, route)? {
            None => None,
            Some(HttpFaceBlock::Endpoint) => Some(Reason::ModelFence),
            Some(HttpFaceBlock::Grant) => Some(Reason::NotGranted),
        },
    )
}

enum HttpFaceBlock {
    Endpoint,
    Grant,
}

fn http_face_block(
    tx: &Transaction<'_>,
    row: &Admission,
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
    route: &NormalizedRoute,
) -> Result<Option<HttpFaceBlock>, PolicyFault> {
    let Some(url) = face_url(destination, model, protocol, &row.provider_id) else {
        return Ok(Some(HttpFaceBlock::Endpoint));
    };
    let Some(origin) = face_origin(&url) else {
        return Ok(Some(HttpFaceBlock::Endpoint));
    };
    let fingerprint = endpoint_fingerprint(&url);
    let Some(endpoint_id) = endpoint_identity(
        destination,
        model,
        protocol,
        &row.provider_id,
        &row.connection_id,
    ) else {
        return Ok(Some(HttpFaceBlock::Endpoint));
    };
    if protocol.as_str() != route.protocol
        || endpoint_id != route.endpoint_id
        || origin != route.origin
        || fingerprint != route.endpoint_fingerprint
        || fingerprint.is_empty()
        || origin.is_empty()
    {
        return Ok(Some(HttpFaceBlock::Endpoint));
    }
    let grants = read_route_grants(tx, &row.credential_id)?;
    let sealed = !matches!(destination.adapter, AdapterKind::Http | AdapterKind::Cpa);
    let auth_none = destination.auth_scheme == AuthScheme::None;
    if !grants_cover(&grants, &endpoint_id, &origin, &url, sealed, auth_none) {
        return Ok(Some(HttpFaceBlock::Grant));
    }
    Ok(None)
}

fn validation_of(row: &Admission) -> ValidationRow {
    ValidationRow {
        credential_id: row.credential_id.clone(),
        credential_version: row.version,
        enabled: row.credential_enabled,
        binding_enabled: row.binding_enabled,
        setup_step: row.setup_step.clone(),
        account_type: row.account_type.clone(),
        material_present: row.material_present,
        destination_enabled: row.destination_enabled,
        destination_draft: row.destination_draft,
        registration_complete: registration_is_complete(&row.account_type, &row.setup_step),
    }
}

fn load_admission(
    tx: &Transaction<'_>,
    credential_id: &str,
) -> Result<Option<Admission>, PolicyFault> {
    let row = tx
        .query_row(
            "SELECT c.id, COALESCE(c.provider_id, ''), COALESCE(c.destination_id, ''),
                    COALESCE(c.binding_id, ''), COALESCE(c.credential_version, 0),
                    COALESCE(c.enabled, 0), COALESCE(c.binding_enabled, 1),
                    COALESCE(c.setup_step, ''), COALESCE(c.account_type, 'key'),
                    COALESCE(c.key_cipher, ''), COALESCE(c.scope_json, ''),
                    COALESCE(c.authorization_connection_id, ''),
                    COALESCE(d.enabled, 0), COALESCE(d.onboarding_draft, 0)
             FROM credentials c
             LEFT JOIN destinations d ON d.id = c.destination_id
             WHERE c.id = ?1",
            [credential_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    let Some((
        credential_id,
        provider_id,
        destination_id,
        binding_id,
        version,
        enabled,
        binding_enabled,
        setup_step,
        account_type,
        key_cipher,
        scope_json,
        connection_id,
        destination_enabled,
        destination_draft,
    )) = row
    else {
        return Ok(None);
    };
    if version < 0 {
        return Err(PolicyFault::Malformed);
    }
    let pending_setup = match setup_step.trim() {
        "" | "ready" => false,
        _ => true,
    };
    Ok(Some(Admission {
        credential_id,
        provider_id,
        destination_id,
        binding_id,
        version: version as u64,
        credential_enabled: enabled != 0,
        binding_enabled: binding_enabled != 0,
        setup_step,
        account_type,
        material_present: !key_cipher.trim().is_empty(),
        destination_enabled: destination_enabled != 0,
        destination_draft: destination_draft != 0,
        connection_id,
        scope_json,
        pending_setup,
    }))
}

fn load_destination(
    tx: &Transaction<'_>,
    destination_id: &str,
) -> Result<Option<Destination>, PolicyFault> {
    if destination_id.is_empty() {
        return Ok(None);
    }
    let row = tx
        .query_row(
            "SELECT id, legacy_kind, legacy_id, adapter, name, brand_family, base_url,
                    protocols_json, auth_scheme, model_resolution, capabilities_json, plan_json,
                    max_credentials, observer_credential_id, enabled
             FROM destinations WHERE id = ?1",
            [destination_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<i64>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, i64>(14)?,
                ))
            },
        )
        .optional()
        .map_err(|_| PolicyFault::Unavailable)?;
    let Some((
        id,
        legacy_kind,
        legacy_id,
        adapter,
        name,
        brand_family,
        base_url,
        protocols_json,
        auth_scheme,
        model_resolution,
        capabilities_json,
        plan_json,
        max_credentials,
        observer_credential_id,
        enabled,
    )) = row
    else {
        return Ok(None);
    };
    let catalog = crate::db::destination_store::load_destination_catalog(tx, &id)
        .map_err(|_| PolicyFault::Unavailable)?;
    let protocol_routes = crate::db::destination_store::load_protocol_routes(tx, &id)
        .map_err(|_| PolicyFault::Unavailable)?;
    let destination = Destination {
        id,
        legacy: legacy_ref(&legacy_kind, legacy_id).ok_or(PolicyFault::Unavailable)?,
        adapter: adapter_kind(&adapter).ok_or(PolicyFault::Unavailable)?,
        name,
        brand_family,
        base_url,
        protocols: serde_json::from_str(&protocols_json).map_err(|_| PolicyFault::Unavailable)?,
        protocol_routes,
        auth_scheme: auth_scheme_kind(&auth_scheme).ok_or(PolicyFault::Unavailable)?,
        model_resolution: resolution_kind(&model_resolution).ok_or(PolicyFault::Unavailable)?,
        catalog,
        capabilities: serde_json::from_str::<Capabilities>(&capabilities_json)
            .map_err(|_| PolicyFault::Unavailable)?,
        plan: plan_json
            .as_deref()
            .map(serde_json::from_str::<Plan>)
            .transpose()
            .map_err(|_| PolicyFault::Unavailable)?,
        max_credentials: max_credentials
            .map(|value| u32::try_from(value).map_err(|_| PolicyFault::Unavailable))
            .transpose()?,
        observer_credential_id,
        enabled: enabled != 0,
    };
    crate::db::http_routes::validate_loaded_destination(&destination)
        .map_err(|_| PolicyFault::Unavailable)?;
    Ok(Some(destination))
}

fn legacy_ref(kind: &str, id: String) -> Option<LegacyDestinationRef> {
    match kind {
        "builtin" => Some(LegacyDestinationRef::Builtin(id)),
        "dynamic" => Some(LegacyDestinationRef::Dynamic(id)),
        "custom_account" => Some(LegacyDestinationRef::CustomAccount(id)),
        "platform_parent" => Some(LegacyDestinationRef::PlatformParent(id)),
        _ => None,
    }
}

fn adapter_kind(value: &str) -> Option<AdapterKind> {
    AdapterKind::ALL
        .into_iter()
        .find(|kind| kind.as_str() == value)
}

fn auth_scheme_kind(value: &str) -> Option<AuthScheme> {
    match value {
        "none" => Some(AuthScheme::None),
        "bearer" => Some(AuthScheme::Bearer),
        "x_api_key" => Some(AuthScheme::XApiKey),
        "api_key" => Some(AuthScheme::ApiKey),
        _ => None,
    }
}

fn resolution_kind(value: &str) -> Option<ModelResolution> {
    match value {
        "adapter_defined" => Some(ModelResolution::AdapterDefined),
        "public_only" => Some(ModelResolution::PublicOnly),
        "public_and_upstream" => Some(ModelResolution::PublicAndUpstream),
        _ => None,
    }
}

fn catalog_row<'a>(catalog: &'a [CatalogModel], upstream: &str) -> Option<&'a CatalogModel> {
    let mut matched = catalog
        .iter()
        .filter(|model| model.enabled && model_ids_match(&model.upstream_model, upstream));
    let model = matched.next()?;
    matched.next().is_none().then_some(model)
}

fn name_resolves(provider_id: &str, model: &CatalogModel, public_name: &str) -> bool {
    let alias = curated_alias(provider_id, &model.upstream_model);
    model_ids_match(public_name, &model.public_model)
        || model_ids_match(public_name, &model.upstream_model)
        || (!alias.is_empty() && model_ids_match(public_name, &alias))
}

fn curated_alias(provider_id: &str, upstream: &str) -> String {
    if !matches!(
        provider_id,
        OPENCODE_PROVIDER_ID
            | OPENCODE_ZEN_FREE_PROVIDER_ID
            | COMMAND_CODE_PROVIDER_ID
            | MINIMAX_PROVIDER_ID
            | KIMI_PROVIDER_ID
            | OLLAMA_PROVIDER_ID
    ) {
        return String::new();
    }
    ocg_gateway::alias::canonical_alias_for_provider_model(provider_id, upstream, &[], &[])
}

fn scope_allows(raw: &str, public_model: &str) -> bool {
    let Ok(scope) = serde_json::from_str::<ModelScope>(raw) else {
        return false;
    };
    match scope {
        ModelScope::All => true,
        ModelScope::Only { models } => {
            !models.is_empty()
                && models
                    .iter()
                    .any(|name| model_ids_match(name, public_model))
        }
    }
}

fn protocol_kind(value: &str) -> Option<UpstreamProtocolKind> {
    UpstreamProtocolKind::try_from(value).ok()
}

fn api_format(protocol: UpstreamProtocolKind) -> ApiFormat {
    match protocol {
        UpstreamProtocolKind::ChatCompletions => ApiFormat::ChatCompletions,
        UpstreamProtocolKind::Responses => ApiFormat::Responses,
        UpstreamProtocolKind::Messages => ApiFormat::Messages,
    }
}

fn protocol_open(
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
) -> bool {
    let protocols = if model.upstream_override.is_some() || model.protocols.is_empty() {
        http_model_protocols(destination, model)
    } else {
        model.protocols.clone()
    };
    protocols.contains(&protocol)
}

enum GrantRows {
    Closed,
    Open {
        endpoints: Vec<String>,
        origins: Vec<String>,
    },
}

fn read_route_grants(tx: &Transaction<'_>, credential_id: &str) -> Result<GrantRows, PolicyFault> {
    if !table_exists(tx, "credential_grants")? {
        return Ok(GrantRows::Closed);
    }
    let mut statement = tx
        .prepare(
            "SELECT kind, value FROM credential_grants WHERE credential_id = ?1 ORDER BY rowid",
        )
        .map_err(|_| PolicyFault::Unavailable)?;
    let rows = statement
        .query_map([credential_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut endpoints = Vec::new();
    let mut origins = Vec::new();
    for row in rows {
        let (kind, value) = row.map_err(|_| PolicyFault::Unavailable)?;
        match kind.as_str() {
            "endpoint_id" => endpoints.push(value),
            "origin" => origins.push(value),
            _ => return Ok(GrantRows::Closed),
        }
    }
    Ok(GrantRows::Open { endpoints, origins })
}

fn grants_cover(
    grants: &GrantRows,
    endpoint_id: &str,
    origin: &str,
    url: &str,
    sealed: bool,
    auth_none: bool,
) -> bool {
    let GrantRows::Open { endpoints, origins } = grants else {
        return false;
    };
    if endpoint_id.is_empty() || !endpoints.iter().any(|id| id == endpoint_id) {
        return false;
    }
    if origins.is_empty() {
        return sealed || auth_none;
    }
    origins
        .iter()
        .any(|item| origins_equivalent(item, origin) || origins_equivalent(item, url))
}

fn face_url(
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
    provider_id: &str,
) -> Option<String> {
    let raw = if destination.adapter == AdapterKind::Http {
        let route = http_model_route(destination, model, protocol)?;
        resolve_http_face(&route.endpoint_url, protocol)?
    } else if destination.adapter == AdapterKind::Cpa {
        return None;
    } else if let Some(route) = http_model_route(destination, model, protocol)
        && !route.endpoint_url.trim().is_empty()
        && model.upstream_override.is_some()
    {
        resolve_http_face(&route.endpoint_url, protocol)?
    } else {
        if !stored_base_allowed(destination) {
            return None;
        }
        let plan = crate::provider::builtin_provider(provider_id)?;
        if !plan.upstream_protocols.contains(&protocol) {
            return None;
        }
        let joined = sealed_raw_url(destination.adapter, protocol)?;
        rewrite_test_url(destination.adapter, joined)?
    };
    canonical_url(&raw)
}

fn sealed_raw_url(adapter: AdapterKind, protocol: UpstreamProtocolKind) -> Option<String> {
    let url = match adapter {
        AdapterKind::OpencodeGo => join_url(
            crate::provider::OPENCODE_GO_BASE_URL,
            api_format(protocol)
                .upstream_path()
                .unwrap_or("/v1/chat/completions"),
        ),
        AdapterKind::Zen => join_url(
            crate::provider::OPENCODE_ZEN_BASE_URL,
            api_format(protocol)
                .upstream_path()
                .unwrap_or("/v1/chat/completions"),
        ),
        AdapterKind::Goat => {
            let path = command_code_upstream_path(api_format(protocol))?;
            join_url(crate::provider::COMMAND_CODE_GOAT_BASE_URL, path)
        }
        AdapterKind::Minimax => match protocol {
            UpstreamProtocolKind::ChatCompletions => join_url(
                crate::provider::MINIMAX_CN_BASE_URL,
                crate::provider::MINIMAX_CN_CHAT_COMPLETIONS_PATH,
            ),
            UpstreamProtocolKind::Responses => join_url(
                crate::provider::MINIMAX_CN_BASE_URL,
                crate::provider::MINIMAX_CN_RESPONSES_PATH,
            ),
            UpstreamProtocolKind::Messages => join_url(
                crate::provider::MINIMAX_CN_ANTHROPIC_BASE_URL,
                crate::provider::MINIMAX_CN_MESSAGES_PATH,
            ),
        },
        AdapterKind::Kimi => {
            let path = match protocol {
                UpstreamProtocolKind::ChatCompletions => {
                    crate::provider::KIMI_CN_CHAT_COMPLETIONS_PATH
                }
                UpstreamProtocolKind::Messages => crate::provider::KIMI_CN_MESSAGES_PATH,
                UpstreamProtocolKind::Responses => return None,
            };
            join_url(crate::provider::KIMI_CN_BASE_URL, path)
        }
        AdapterKind::Ollama => {
            if protocol != UpstreamProtocolKind::ChatCompletions {
                return None;
            }
            join_url(
                crate::provider::OLLAMA_CLOUD_BASE_URL,
                crate::provider::OLLAMA_CLOUD_CHAT_COMPLETIONS_PATH,
            )
        }
        AdapterKind::Http | AdapterKind::Cpa => return None,
    };
    Some(url)
}

fn stored_base_allowed(destination: &Destination) -> bool {
    let Some(stored) = destination
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return true;
    };
    UpstreamProtocolKind::ALL
        .into_iter()
        .filter_map(|protocol| official_base(destination.adapter, protocol))
        .any(|official| crate::custom_http::origins_match(stored, official))
}

fn official_base(adapter: AdapterKind, protocol: UpstreamProtocolKind) -> Option<&'static str> {
    match adapter {
        AdapterKind::OpencodeGo => Some(crate::provider::OPENCODE_GO_BASE_URL),
        AdapterKind::Zen => Some(crate::provider::OPENCODE_ZEN_BASE_URL),
        AdapterKind::Goat => Some(crate::provider::COMMAND_CODE_GOAT_BASE_URL),
        AdapterKind::Minimax => match protocol {
            UpstreamProtocolKind::Messages => Some(crate::provider::MINIMAX_CN_ANTHROPIC_BASE_URL),
            _ => Some(crate::provider::MINIMAX_CN_BASE_URL),
        },
        AdapterKind::Kimi => Some(crate::provider::KIMI_CN_BASE_URL),
        AdapterKind::Ollama => Some(crate::provider::OLLAMA_CLOUD_BASE_URL),
        AdapterKind::Http | AdapterKind::Cpa => None,
    }
}

fn rewrite_test_url(adapter: AdapterKind, url: String) -> Option<String> {
    let provider = match adapter {
        AdapterKind::OpencodeGo => crate::provider::OPENCODE_PROVIDER_ID,
        AdapterKind::Goat => crate::provider::COMMAND_CODE_PROVIDER_ID,
        _ => return Some(url),
    };
    match crate::cpa_test_endpoints::rewrite_url(provider, &url) {
        Ok(Some(rewritten)) => Some(rewritten),
        Ok(None) => Some(url),
        Err(_) => None,
    }
}

fn endpoint_identity(
    destination: &Destination,
    model: &CatalogModel,
    protocol: UpstreamProtocolKind,
    provider_id: &str,
    connection_id: &str,
) -> Option<String> {
    let connection = parse_connection(connection_id)?;
    let grant_routes = if destination.adapter != AdapterKind::Http
        && let Some(plan) = crate::provider::builtin_provider(provider_id)
    {
        plan.upstream_protocols
            .iter()
            .copied()
            .map(|protocol| RouteSpec {
                operation: EndpointOperation::from(protocol),
                url: None,
            })
            .collect::<Vec<_>>()
    } else {
        http_configured_routes(destination)
    };
    let assigned = assigned_endpoints_for_routes(&connection, &grant_routes);
    let stored = http_model_route(destination, model, protocol).map(|route| route.endpoint_url);
    pick_endpoint(&assigned, &grant_routes, protocol, stored.as_deref())
}

fn pick_endpoint(
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

fn parse_connection(raw: &str) -> Option<ConnectionId> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

fn face_origin(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let port = parsed.port_or_known_default()?;
    Some(format!(
        "{}://{host}:{port}",
        parsed.scheme().to_ascii_lowercase()
    ))
}

fn canonical_url(value: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(value.trim()).ok()?;
    crate::custom_http::inspect_custom_url(&parsed).ok()?;
    if parsed.fragment().is_some() {
        return None;
    }
    Some(parsed.as_str().to_string())
}

fn resolve_http_face(source: &str, protocol: UpstreamProtocolKind) -> Option<String> {
    let (bare, query) = split_query(source)?;
    let resolved = crate::custom_http::resolve_custom_endpoints(&bare, protocol).ok()?;
    attach_query(resolved.inference.as_str(), query.as_deref())
}

fn split_query(value: &str) -> Option<(String, Option<String>)> {
    let mut parsed = reqwest::Url::parse(value.trim()).ok()?;
    crate::custom_http::inspect_custom_url(&parsed).ok()?;
    if parsed.fragment().is_some() {
        return None;
    }
    let query = parsed.query().map(str::to_string);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some((parsed.as_str().trim_end_matches('/').to_string(), query))
}

fn attach_query(value: &str, query: Option<&str>) -> Option<String> {
    let mut parsed = reqwest::Url::parse(value.trim()).ok()?;
    if let Some(query) = query {
        parsed.set_query(Some(query));
    }
    if parsed.fragment().is_some() {
        return None;
    }
    crate::custom_http::inspect_custom_url(&parsed).ok()?;
    Some(parsed.as_str().to_string())
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn oauth_rows<'a>(record: &'a Record, credential_id: &str) -> Vec<&'a store::OAuthStamp> {
    if credential_id.is_empty() {
        return Vec::new();
    }
    record
        .oauth
        .iter()
        .filter(|stamp| stamp.credential_id == credential_id)
        .collect()
}

fn oauth_presence_blocks(record: &Record, credential_id: &str) -> bool {
    let rows = oauth_rows(record, credential_id);
    if rows.is_empty() {
        return false;
    }
    rows.len() != 1
        || matches!(
            rows[0].presence,
            store::OAuthPresence::Pending | store::OAuthPresence::Absent
        )
}

fn present_native<'a>(record: &'a Record, row: &Admission) -> Option<&'a store::OAuthStamp> {
    let rows = oauth_rows(record, &row.credential_id);
    if rows.len() != 1 {
        return None;
    }
    let stamp = rows[0];
    if stamp.presence != store::OAuthPresence::Present
        || stamp.credential_version != row.version
        || row.binding_id.is_empty()
        || stamp.auth_id.is_empty()
        || !endpoint_pin_capability_listed(&record.host_capabilities)
    {
        None
    } else {
        Some(stamp)
    }
}

fn authentication_satisfied(
    tx: &Transaction<'_>,
    record: &Record,
    row: &Admission,
    cipher: &dyn KeyCipher,
) -> Result<bool, PolicyFault> {
    let Some(destination) = load_destination(tx, &row.destination_id)? else {
        return Ok(false);
    };
    if destination.adapter == AdapterKind::Cpa || !oauth_rows(record, &row.credential_id).is_empty()
    {
        return Ok(present_native(record, row).is_some());
    }
    if destination.auth_scheme == AuthScheme::None {
        return Ok(!row.material_present);
    }
    keyed_material_decrypts(tx, &row.credential_id, cipher)
}

fn keyed_material_decrypts(
    tx: &Transaction<'_>,
    credential_id: &str,
    cipher: &dyn KeyCipher,
) -> Result<bool, PolicyFault> {
    let Some(row) = load_row(tx, credential_id)? else {
        return Ok(false);
    };
    if row.key_cipher.trim().is_empty() {
        return Ok(false);
    }
    match cipher.decrypt(&row.key_cipher) {
        Ok(mut plain) => {
            let satisfied = !plain.trim().is_empty();
            plain.zeroize();
            Ok(satisfied)
        }
        Err(_) => Ok(false),
    }
}

fn native_authority(stamp: &store::OAuthStamp) -> NativeAuthorityFacts {
    NativeAuthorityFacts {
        raw_label: stamp.raw_provider_label.clone(),
        provider: stamp.native_provider.clone(),
        mode: stamp.native_mode.clone(),
        reported_base: stamp.reported_base.clone(),
    }
}

fn owned_native_connection() -> ConnectionId {
    connection_id_for_legacy(
        LegacyConnectionKind::BuiltinProvider,
        crate::db::native_binding::OWNED_NATIVE_LEGACY_ID,
    )
}

enum NativeFaceBlock {
    Presence,
    Alignment,
    Origin,
    Mode,
    Targets,
    Grant,
}

fn confirm_native_face(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    row: &Admission,
    route: &NormalizedRoute,
) -> Result<Option<Reason>, PolicyFault> {
    Ok(match native_face_block(tx, record, map, row, route)? {
        None => None,
        Some(NativeFaceBlock::Presence | NativeFaceBlock::Alignment) => Some(Reason::IdentityFence),
        Some(NativeFaceBlock::Grant) => Some(Reason::NotGranted),
        Some(NativeFaceBlock::Origin | NativeFaceBlock::Mode | NativeFaceBlock::Targets) => {
            Some(Reason::ModelFence)
        }
    })
}

fn native_face_block(
    tx: &Transaction<'_>,
    record: &Record,
    map: &[AuthStamp],
    row: &Admission,
    route: &NormalizedRoute,
) -> Result<Option<NativeFaceBlock>, PolicyFault> {
    let Some(stamp) = present_native(record, row) else {
        return Ok(Some(NativeFaceBlock::Presence));
    };
    let aligned = map.iter().any(|auth| {
        auth.credential_id == stamp.credential_id
            && auth.credential_version == stamp.credential_version
            && auth.binding_id == row.binding_id
            && auth.auth_id == stamp.auth_id
            && auth.registration_epoch == stamp.registration_epoch
    });
    if !aligned {
        return Ok(Some(NativeFaceBlock::Alignment));
    }
    let Some(canonical_origin) = canonical_native_url(&route.origin) else {
        return Ok(Some(NativeFaceBlock::Origin));
    };
    if ocg_domain::credential::normalize_origin(&canonical_origin).as_deref()
        != Some(route.origin.as_str())
    {
        return Ok(Some(NativeFaceBlock::Origin));
    }
    let facts = native_authority(stamp);
    let connection = owned_native_connection();
    let Some((primary, targets)) =
        native_route_targets(&facts, &route.upstream_model, &route.protocol, &connection)
    else {
        return Ok(Some(NativeFaceBlock::Mode));
    };
    if canonical_native_url(&primary.origin).as_deref() != Some(primary.origin.as_str())
        || primary.protocol != route.protocol
        || primary.endpoint_id != route.endpoint_id
        || primary.origin != route.origin
        || primary.endpoint_fingerprint != route.endpoint_fingerprint
        || primary.http_method != "POST"
        || !stored_targets_match(&route.native_targets, &targets)
    {
        return Ok(Some(NativeFaceBlock::Targets));
    }
    let grants = read_route_grants(tx, &row.credential_id)?;
    if !grants_cover(
        &grants,
        &primary.endpoint_id,
        &primary.origin,
        &primary.origin,
        true,
        false,
    ) {
        return Ok(Some(NativeFaceBlock::Grant));
    }
    Ok(None)
}

fn stored_targets_match(
    stored: &[NativeDispatchTarget],
    canonical: &[NativeDispatchTarget],
) -> bool {
    if stored.is_empty() || !accept_stored_native_targets(stored) {
        return false;
    }
    stored.iter().all(|item| {
        canonical.iter().any(|expected| {
            expected.pin == item.pin
                && item
                    .generation_kinds
                    .iter()
                    .all(|kind| expected.generation_kinds.iter().any(|saved| saved == kind))
        })
    })
}

fn granted_endpoint_pins(
    facts: &LiveFacts,
    tx: &Transaction<'_>,
    identity: &AttemptIdentity,
) -> Result<Option<Vec<NativeEndpointPin>>, PolicyFault> {
    if facts.now >= facts.deadline {
        return Err(PolicyFault::Unavailable);
    }
    let Some(captured) = facts.attempt.as_ref() else {
        return Err(PolicyFault::Unavailable);
    };
    if captured.attempt_id != identity.attempt_id
        || captured.callable_protocol.is_empty()
        || !CALLABLE_PROTOCOLS.contains(&captured.callable_protocol.as_str())
        || !GENERATION_KINDS.contains(&captured.generation_kind.as_str())
    {
        return Err(PolicyFault::Unavailable);
    }
    let record = store::load_tx(tx).map_err(|_| PolicyFault::Unavailable)?;
    if oauth_presence_blocks(&record, &identity.credential_id) {
        return Err(PolicyFault::Unavailable);
    }
    if oauth_rows(&record, &identity.credential_id).is_empty() {
        return Ok(None);
    }
    let Some(row) = load_admission(tx, &identity.credential_id)? else {
        return Err(PolicyFault::Unavailable);
    };
    let Some(stamp) = present_native(&record, &row) else {
        return Err(PolicyFault::Unavailable);
    };
    if stamp.registration_epoch != identity.registration_epoch
        || stamp.auth_id != identity.auth_id
        || stamp.credential_version != identity.credential_version
        || row.binding_id != identity.binding_id
        || row.binding_id.is_empty()
        || !scope_allows(&row.scope_json, &identity.public_model)
    {
        return Err(PolicyFault::Unavailable);
    }
    let mode = resolved_mode(facts.mode, facts.ready.as_ref(), &record);
    let (map, routes) = authority(&record, mode);
    let material = current_material(tx, &record, &identity.credential_id, facts.cipher.as_ref())?;
    let registration_epoch = adopted_registration_epoch(
        map,
        &row.credential_id,
        row.version,
        &row.binding_id,
        adopt_stamp_for(
            tx,
            &record,
            &row.credential_id,
            &row.destination_id,
            row.material_present,
        )?,
        &material,
    );
    let live = CurrentCredential {
        credential_id: row.credential_id.clone(),
        credential_version: row.version,
        provider_id: row.provider_id.clone(),
        binding_id: row.binding_id.clone(),
        material_revision: material.revision,
        registration_epoch,
        auth_id: material.auth_id,
        memberships: Vec::new(),
        scopes: Vec::new(),
        granted: true,
        enabled: row.credential_enabled,
        deleted: false,
        rebound: false,
    };
    let located = match locate(map, routes, &row, &live)? {
        Ok(located) => located,
        Err(_) => return Err(PolicyFault::Unavailable),
    };
    let Some(route) = one_protocol(
        located.set,
        &identity.public_model,
        &captured.callable_protocol,
    ) else {
        return Err(PolicyFault::Unavailable);
    };
    if !model_ids_match(&route.upstream_model, &identity.upstream_model)
        || route.protocol != captured.callable_protocol
    {
        return Err(PolicyFault::Unavailable);
    }
    if !route.native_targets.is_empty() && !accept_stored_native_targets(&route.native_targets) {
        return Err(PolicyFault::Malformed);
    }
    let matched: Option<Vec<NativeEndpointPin>> = targets_for_applied_route(
        route,
        &captured.callable_protocol,
        &captured.generation_kind,
    );
    match matched {
        Some(pins) if pins.is_empty() && captured.generation_kind == "count-tokens" => {
            prove_local_count(stamp, route, &captured.callable_protocol)
        }
        Some(pins) if pins.is_empty() => Err(PolicyFault::Unavailable),
        Some(pins) => filter_granted_pins(
            tx,
            &identity.credential_id,
            &captured.callable_protocol,
            pins,
        ),
        None => Err(PolicyFault::Unavailable),
    }
}

fn select_granted_pins(
    grants: &GrantRows,
    protocol: &str,
    pins: Vec<NativeEndpointPin>,
) -> Result<Vec<NativeEndpointPin>, PolicyFault> {
    if pins.is_empty() || pins.len() > MAX_NATIVE_TARGETS {
        return Err(PolicyFault::Malformed);
    }
    let mut seen = Vec::new();
    let mut granted_pins = Vec::new();
    for pin in pins {
        if pin.protocol != protocol
            || pin.http_method != "POST"
            || pin.endpoint_id.is_empty()
            || pin.origin.is_empty()
            || pin.endpoint_fingerprint.len() != 64
            || seen.iter().any(|saved: &NativeEndpointPin| saved == &pin)
        {
            return Err(PolicyFault::Malformed);
        }
        seen.push(pin.clone());
        if grants_cover(
            grants,
            &pin.endpoint_id,
            &pin.origin,
            &pin.origin,
            true,
            false,
        ) {
            granted_pins.push(pin);
        }
    }
    Ok(granted_pins)
}

fn filter_granted_pins(
    tx: &Transaction<'_>,
    credential_id: &str,
    protocol: &str,
    pins: Vec<NativeEndpointPin>,
) -> Result<Option<Vec<NativeEndpointPin>>, PolicyFault> {
    let grants = read_route_grants(tx, credential_id)?;
    let granted_pins = select_granted_pins(&grants, protocol, pins)?;
    if granted_pins.is_empty() {
        Err(PolicyFault::Unavailable)
    } else {
        Ok(Some(granted_pins))
    }
}

fn prove_local_count(
    stamp: &store::OAuthStamp,
    route: &NormalizedRoute,
    protocol: &str,
) -> Result<Option<Vec<NativeEndpointPin>>, PolicyFault> {
    let connection = owned_native_connection();
    match native_targets_for(
        &native_authority(stamp),
        &route.upstream_model,
        protocol,
        NativeSourceOperation::CountTokens,
        &connection,
    ) {
        NativeTargetOutcome::LocalOnly => Ok(None),
        NativeTargetOutcome::Network(_) | NativeTargetOutcome::Unavailable => {
            Err(PolicyFault::Unavailable)
        }
    }
}

/// Applied-route pin for one validated protocol. Refusals are unavailable.
/// The native root calls this; it does not advance refresh metadata.
pub(super) fn validated_route_pin_on(
    tx: &rusqlite::Transaction<'_>,
    record: &super::store::Record,
    credential_id: &str,
    credential_version: u64,
    public_model: &str,
    protocol: &str,
    cipher: &dyn ocg_infra::crypto::KeyCipher,
) -> Result<
    (
        super::store::AuthStamp,
        crate::cpa_projection::NormalizedRoute,
    ),
    PolicyFault,
> {
    if public_model.is_empty() || protocol.is_empty() || credential_id.is_empty() {
        return Err(PolicyFault::Unavailable);
    }
    let Some(row) = load_admission(tx, credential_id)? else {
        return Err(PolicyFault::Unavailable);
    };
    if row.version != credential_version {
        return Err(PolicyFault::Unavailable);
    }
    let satisfied = authentication_satisfied(tx, record, &row, cipher)?;
    let Some(kind) = classify_validation_with_auth(&validation_of(&row), satisfied) else {
        return Err(PolicyFault::Unavailable);
    };
    let material = current_material(tx, record, credential_id, cipher)?;
    let registration_epoch = adopted_registration_epoch(
        &record.applied_auth,
        &row.credential_id,
        row.version,
        &row.binding_id,
        adopt_stamp_for(
            tx,
            record,
            &row.credential_id,
            &row.destination_id,
            row.material_present,
        )?,
        &material,
    );
    let live = CurrentCredential {
        credential_id: row.credential_id.clone(),
        credential_version: row.version,
        provider_id: row.provider_id.clone(),
        binding_id: row.binding_id.clone(),
        material_revision: material.revision,
        registration_epoch,
        auth_id: material.auth_id,
        memberships: Vec::new(),
        scopes: Vec::new(),
        granted: false,
        enabled: row.credential_enabled,
        deleted: false,
        rebound: false,
    };
    let located = match locate(&record.applied_auth, &record.applied_routes, &row, &live)? {
        Ok(located) => located,
        Err(_) => return Err(PolicyFault::Unavailable),
    };
    let Some(route) = one_protocol(located.set, public_model, protocol) else {
        return Err(PolicyFault::Unavailable);
    };
    if !validated_route_allowed(kind, row.destination_draft, route.validation_only) {
        return Err(PolicyFault::Unavailable);
    }
    if confirm_one(tx, record, &record.applied_auth, &row, route, public_model)?.is_some() {
        return Err(PolicyFault::Unavailable);
    }
    Ok((located.stamp.clone(), route.clone()))
}

#[cfg(test)]
pub(super) fn preview_stamp(
    tx: &Transaction<'_>,
    legacy_account_id: &str,
    cipher: &dyn KeyCipher,
) -> Result<AuthStamp, ExecutionError> {
    let credential_id: String = tx
        .query_row(
            "SELECT id FROM credentials WHERE legacy_account_id = ?1",
            [legacy_account_id],
            |row| row.get(0),
        )
        .map_err(|_| ExecutionError::Invalid("credential is missing".into()))?;
    let record = store::load_tx(tx).unwrap_or_else(|_| Record::empty());
    let row = load_row(tx, &credential_id)
        .map_err(|_| ExecutionError::Unavailable("credential could not be read".into()))?;
    let Some(row) = row else {
        return Err(ExecutionError::Invalid("credential is missing".into()));
    };
    let material = current_material(tx, &record, &credential_id, cipher)
        .map_err(|_| ExecutionError::Unavailable("credential material could not be read".into()))?;
    Ok(AuthStamp {
        auth_id: material.auth_id,
        credential_id,
        credential_version: row.version,
        binding_id: row.binding_id,
        material_revision: material.revision,
        provider_id: row.provider_id,
        registration_epoch: material.epoch,
    })
}

pub(super) mod explain_resolution;

mod authority;

pub(super) use authority::{
    ConfigAuthorityQuery, ConfigExclusion, ConfigPlane, MaterialPosture, NativeGrantDisposition,
    NativeOperationFact, ProductChannel, RouteAuthorityProof, StaticPosture,
    configuration_authority_on,
};

#[cfg(test)]
mod tests;
