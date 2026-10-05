//! Admit and result decisions. No credential selection and no outer retry.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::evidence::{self, Evidence, Observation};
use super::store::{
    AdmittedAttempt, EvidenceSource, InflightAdmit, PolicyDocument, PolicyFault, Reset,
    Restriction, Scope, Subject, UnknownRecovery, Window, remember_request,
};
use super::wire::{
    Action, AttemptFields, AttemptIdentity, CurrentCredential, Decision, DeclaredSubject,
    ErrorCode, OpportunityPolicy, Outcome, PolicyFacts, PoolMembership, Reason, RequestKind,
    ResultBody, ResultObservation,
};

/// Definite failover statuses. Ordered-route 404 stays stop: no frozen body
/// marker proves the endpoint refused this generation.
const DEFINITE_STATUSES: [u16; 3] = [401, 403, 429];
/// How long an admitted attempt stays correlatable after its execution deadline.
/// Admission itself stops at the deadline. This grace is only for one result publication.
pub(crate) const RESULT_PUBLICATION_GRACE_SECONDS: i64 = 60;

struct Stamp {
    id: String,
    fetched_at: DateTime<Utc>,
}

pub fn projection_fence(
    request: &super::wire::ParsedRequest,
    facts: &PolicyFacts,
) -> Option<Reason> {
    let projection = &request.projection;
    if projection.process_generation != facts.applied.process_generation
        || projection.projection_revision != facts.applied.revision
        || projection.projection_digest != facts.applied.digest
    {
        return Some(Reason::ProjectionFence);
    }
    identity_fence(request, facts)
}

/// Send identity for a result. The captured projection is checked against the
/// admitted row, not against a projection applied after reload.
fn identity_fence(request: &super::wire::ParsedRequest, facts: &PolicyFacts) -> Option<Reason> {
    if matches!(request.kind, RequestKind::Ready) && request.attempt.is_none() {
        return None;
    }
    let Some(wire) = request.attempt.as_ref() else {
        return Some(Reason::Malformed);
    };
    let Some(attempt) = facts.attempt.as_ref() else {
        return Some(Reason::Malformed);
    };
    if wire.request_id != attempt.request_id {
        return Some(Reason::RequestFence);
    }
    if !wire_matches_attempt(wire, attempt) {
        return Some(Reason::IdentityFence);
    }
    if wire.public_model != attempt.public_model
        || !same_executed_upstream(
            &attempt.public_model,
            &attempt.upstream_model,
            &wire.upstream_model,
        )
    {
        return Some(Reason::ModelFence);
    }
    if facts
        .current
        .as_ref()
        .is_some_and(|current| wire.provider_id != current.provider_id)
    {
        return Some(Reason::IdentityFence);
    }
    None
}

/// Reasons that do not consume an admitted attempt. Observation uses the complement.
pub(crate) fn result_consumes_attempt(reason: Reason) -> bool {
    !matches!(
        reason,
        Reason::Uncorrelated
            | Reason::Malformed
            | Reason::IdentityFence
            | Reason::ProjectionFence
            | Reason::RequestFence
            | Reason::ModelFence
            | Reason::Unavailable
            | Reason::Unauthorized
            | Reason::Oversize
    )
}

/// The owned host stores its selection key in the result upstream field. When
/// that key is the admitted public model, the executed upstream is unchanged.
/// Any other reported upstream is a different model.
fn same_executed_upstream(public_model: &str, admitted_upstream: &str, reported: &str) -> bool {
    reported == admitted_upstream || (public_model != admitted_upstream && reported == public_model)
}

fn wire_matches_attempt(wire: &AttemptFields, attempt: &AttemptIdentity) -> bool {
    wire.attempt_id == attempt.attempt_id
        && wire.auth_id == attempt.auth_id
        && wire.credential_id == attempt.credential_id
        && wire.credential_version == attempt.credential_version
        && wire.provider_id == attempt.provider_id
        && wire.registration_epoch == attempt.registration_epoch
        && wire.material_revision == attempt.material_revision
        && wire.kind == attempt.kind
}

pub fn admit(
    document: &mut PolicyDocument,
    request: &super::wire::ParsedRequest,
    facts: &PolicyFacts,
    opportunity: &OpportunityPolicy,
) -> Result<Decision, PolicyFault> {
    sweep_expired(document, facts.now);
    sweep_attempts(document, facts.now);
    sweep_unknown_leases(document, facts.now);
    if let Some(reason) = projection_fence(request, facts) {
        return Ok(Decision::stop(reason));
    }
    if let Some(reason) = facts.caller_stop {
        return Ok(Decision::stop(reason));
    }
    if facts.attempt.is_none() || facts.current.is_none() {
        return Ok(Decision::stop(Reason::Malformed));
    }
    if facts.now >= facts.deadline_at {
        return Ok(Decision::stop(Reason::Deadline));
    }
    if let Some(decision) = current_gate(facts) {
        if facts.validated_pin && decision.action == Action::Skip {
            return Ok(Decision::stop(decision.reason));
        }
        return Ok(decision);
    }
    let blocked_by_unknown = {
        let active = applicable(document, facts);
        if latest_known(&active, facts.now).is_some() {
            return Ok(Decision::skip(
                Reason::Restricted,
                blocking_deadline(&active, facts.now),
            ));
        }
        !unknowns(&active).is_empty()
    };
    if blocked_by_unknown {
        return admit_unknown(document, facts, opportunity);
    }
    let scopes = authorized_scopes(facts)?;
    record_attempt(document, facts, scopes)?;
    Ok(Decision::allow(Reason::Eligible))
}

pub fn result(
    document: &mut PolicyDocument,
    request: &super::wire::ParsedRequest,
    facts: &PolicyFacts,
    body: &ResultBody,
) -> Result<Decision, PolicyFault> {
    let _retry_after = body.retry_after.as_deref();
    sweep_expired(document, facts.now);
    sweep_attempts(document, facts.now);
    sweep_unknown_leases(document, facts.now);
    if let Some(reason) = identity_fence(request, facts) {
        return Ok(Decision::stop(reason));
    }
    if facts.attempt.is_none() || facts.current.is_none() {
        return Ok(Decision::stop(Reason::Malformed));
    }
    let continuation = classify(body, facts.now < facts.deadline_at);
    let Some(admitted) = correlated(document, request, facts) else {
        return Ok(Decision::stop(Reason::Uncorrelated));
    };
    release_inflight(document, admitted.attempt_id);
    let mut recorded = false;
    if should_persist(body) && durable_live(&admitted, facts) {
        if let Some(stamp) = rejection_stamp(body, &admitted, facts) {
            let evidence =
                evidence::rejection(&admitted.provider_id, &body.response_body, stamp.fetched_at);
            if !evidence.is_empty() {
                let scopes = admitted
                    .scopes
                    .iter()
                    .filter(|scope| scope_still_held(scope, facts))
                    .cloned()
                    .collect::<Vec<_>>();
                // An empty authorized list must not claim ExplicitRejection.
                if !scopes.is_empty() {
                    apply_evidence(document, &scopes, &evidence, &stamp, facts.now)?;
                    recorded = evidence.creates_restriction();
                }
            }
        }
    }
    forget_attempt(document, admitted.attempt_id);
    Ok(match continuation {
        Continuation::Next if body.error_code == ErrorCode::Transport => {
            Decision::skip(Reason::UnsentTransport, None)
        }
        Continuation::Next if recorded => Decision::skip(Reason::ExplicitRejection, None),
        Continuation::Next => Decision::skip(Reason::ProviderRejected, None),
        Continuation::Stop(reason) => Decision::stop(reason),
    })
}

pub fn apply_quota(
    document: &mut PolicyDocument,
    facts: &PolicyFacts,
    observation_id: &str,
    fetched_at: DateTime<Utc>,
    evidence: &Evidence,
) -> Result<(), PolicyFault> {
    let scopes = authorized_scopes(facts)?;
    apply_evidence(
        document,
        &scopes,
        evidence,
        &Stamp {
            id: observation_id.to_string(),
            fetched_at,
        },
        facts.now,
    )
}

enum Continuation {
    Next,
    Stop(Reason),
}

fn classify(body: &ResultBody, within_deadline: bool) -> Continuation {
    if body.error_code == ErrorCode::UsageObservation && body.outcome == Outcome::Success {
        return Continuation::Stop(Reason::Completed);
    }
    if body.error_code == ErrorCode::UsageObservation && body.outcome == Outcome::LocalFailure {
        return Continuation::Stop(Reason::UsageObservation);
    }
    match body.outcome {
        Outcome::Cancelled => Continuation::Stop(Reason::Cancelled),
        Outcome::Deadline => Continuation::Stop(Reason::Deadline),
        Outcome::Uncertain => Continuation::Stop(Reason::Uncertain),
        Outcome::Success => {
            // A finished SSE has streamStarted and bodyComplete. Output bars
            // replay only when the body did not complete or the status is missing.
            let lost = !body.body_complete || (body.sent && body.status.is_none());
            if lost {
                Continuation::Stop(Reason::Uncertain)
            } else {
                Continuation::Stop(Reason::Completed)
            }
        }
        Outcome::ExplicitRejection => {
            if definite_rejection(body) {
                Continuation::Next
            } else {
                Continuation::Stop(Reason::Uncertain)
            }
        }
        Outcome::LocalFailure => classify_local(body, within_deadline),
    }
}

fn definite_rejection(body: &ResultBody) -> bool {
    body.sent
        && body.body_complete
        && !body.stream_started
        && body
            .status
            .is_some_and(|status| DEFINITE_STATUSES.contains(&status))
}

fn classify_local(body: &ResultBody, within_deadline: bool) -> Continuation {
    if body.sent || body.stream_started || (body.body_complete && body.status.is_some()) {
        return Continuation::Stop(Reason::Uncertain);
    }
    match body.error_code {
        ErrorCode::Validation => Continuation::Stop(Reason::LocalValidation),
        ErrorCode::Transport if within_deadline => Continuation::Next,
        ErrorCode::Transport => Continuation::Stop(Reason::Deadline),
        _ => Continuation::Stop(Reason::Uncertain),
    }
}

fn should_persist(body: &ResultBody) -> bool {
    if body.error_code == ErrorCode::UsageObservation || body.error_code == ErrorCode::Validation {
        return false;
    }
    if !body.body_complete || body.response_body.is_empty() {
        return false;
    }
    matches!(
        body.outcome,
        Outcome::ExplicitRejection | Outcome::Cancelled | Outcome::Deadline | Outcome::Success
    )
}

fn current_gate(facts: &PolicyFacts) -> Option<Decision> {
    let current = facts.current.as_ref()?;
    let attempt = facts.attempt.as_ref()?;
    if current.deleted {
        return Some(Decision::skip(Reason::Deleted, None));
    }
    if current.rebound {
        return Some(Decision::skip(Reason::Rebound, None));
    }
    if !durable_attempt(attempt, current) {
        return Some(Decision::skip(Reason::StaleCredential, None));
    }
    if attempt.registration_epoch != current.registration_epoch
        || attempt.material_revision != current.material_revision
        || attempt.auth_id != current.auth_id
    {
        return Some(Decision::stop(Reason::IdentityFence));
    }
    if !current.enabled {
        return Some(Decision::skip(Reason::Disabled, None));
    }
    if !current.granted {
        return Some(Decision::skip(Reason::NotGranted, None));
    }
    None
}

fn durable_attempt(attempt: &AttemptIdentity, current: &CurrentCredential) -> bool {
    attempt.credential_id == current.credential_id
        && attempt.credential_version == current.credential_version
        && attempt.provider_id == current.provider_id
        && attempt.binding_id == current.binding_id
}

fn durable_live(admitted: &AdmittedAttempt, facts: &PolicyFacts) -> bool {
    let Some(current) = facts.current.as_ref() else {
        return false;
    };
    // Grant, auth id, and registration epoch authorize a new or updated
    // restriction. Ordinary OAuth refresh keeps the epoch and may change
    // material text, so material stays an attempt fence. MarkResult generation
    // is not registration identity. An epoch or auth-id change is a replacement.
    !current.deleted
        && !current.rebound
        && current.granted
        && admitted.credential_id == current.credential_id
        && admitted.credential_version == current.credential_version
        && admitted.provider_id == current.provider_id
        && admitted.binding_id == current.binding_id
        && admitted.auth_id == current.auth_id
        && admitted.registration_epoch == current.registration_epoch
}

fn correlated(
    document: &PolicyDocument,
    request: &super::wire::ParsedRequest,
    facts: &PolicyFacts,
) -> Option<AdmittedAttempt> {
    let attempt = facts.attempt.as_ref()?;
    let projection = &request.projection;
    document
        .attempts
        .iter()
        .find(|row| {
            row.attempt_id == attempt.attempt_id
                && row.request_id == attempt.request_id
                && row.credential_id == attempt.credential_id
                && row.credential_version == attempt.credential_version
                && row.provider_id == attempt.provider_id
                && row.binding_id == attempt.binding_id
                && row.public_model == attempt.public_model
                && row.upstream_model == attempt.upstream_model
                && row.auth_id == attempt.auth_id
                && row.material_revision == attempt.material_revision
                && row.registration_epoch == attempt.registration_epoch
                && row.process_generation == projection.process_generation
                && row.projection_revision == projection.projection_revision
                && row.projection_digest == projection.projection_digest
                && row.kind == attempt.kind
        })
        .cloned()
}

fn authorized_scopes(facts: &PolicyFacts) -> Result<Vec<Scope>, PolicyFault> {
    let current = facts.current.as_ref().ok_or(PolicyFault::Unavailable)?;
    if current.scopes.is_empty() {
        return Err(PolicyFault::Unavailable);
    }
    let mut scopes = Vec::with_capacity(current.scopes.len());
    for declared in &current.scopes {
        match &declared.subject {
            DeclaredSubject::Credential => scopes.push(Scope {
                subject: subject_of(current),
                public_model: declared.public_model.clone(),
            }),
            DeclaredSubject::Pool {
                pool_id,
                pool_version,
            } => {
                let Some(member) = current.memberships.iter().find(|member| {
                    member.pool_id == *pool_id && member.pool_version == *pool_version
                }) else {
                    return Err(PolicyFault::Unavailable);
                };
                let covered = match &declared.public_model {
                    Some(model) => covers(member, model),
                    None => member.all_models,
                };
                if !covered {
                    return Err(PolicyFault::Unavailable);
                }
                scopes.push(Scope {
                    subject: Subject::Pool {
                        pool_id: pool_id.clone(),
                        pool_version: *pool_version,
                    },
                    public_model: declared.public_model.clone(),
                });
            }
        }
    }
    Ok(scopes)
}

fn subject_of(current: &CurrentCredential) -> Subject {
    Subject::Credential {
        credential_id: current.credential_id.clone(),
        credential_version: current.credential_version,
        provider_id: current.provider_id.clone(),
        binding_id: current.binding_id.clone(),
    }
}

fn covers(member: &PoolMembership, model: &str) -> bool {
    member.all_models || member.public_models.iter().any(|item| item == model)
}

fn scope_still_held(scope: &Scope, facts: &PolicyFacts) -> bool {
    let Some(current) = facts.current.as_ref() else {
        return false;
    };
    match &scope.subject {
        Subject::Credential {
            credential_id,
            credential_version,
            provider_id,
            binding_id,
        } => {
            credential_id == &current.credential_id
                && *credential_version == current.credential_version
                && provider_id == &current.provider_id
                && binding_id == &current.binding_id
                && declared_credential_covers(current, &scope.public_model)
        }
        Subject::Pool {
            pool_id,
            pool_version,
        } => {
            let member_held = current.memberships.iter().any(|member| {
                member.pool_id == *pool_id
                    && member.pool_version == *pool_version
                    && match &scope.public_model {
                        Some(model) => covers(member, model),
                        None => member.all_models,
                    }
            });
            member_held
                && declared_pool_covers(current, pool_id, *pool_version, &scope.public_model)
        }
    }
}

/// A declared all-models scope covers every captured model. One declared model
/// does not cover a captured all-models scope.
fn declared_model_covers(declared: &Option<String>, captured: &Option<String>) -> bool {
    match declared {
        None => true,
        Some(model) => captured.as_ref() == Some(model),
    }
}

fn declared_credential_covers(current: &CurrentCredential, captured: &Option<String>) -> bool {
    current.scopes.iter().any(|declared| {
        matches!(declared.subject, DeclaredSubject::Credential)
            && declared_model_covers(&declared.public_model, captured)
    })
}

fn declared_pool_covers(
    current: &CurrentCredential,
    pool_id: &str,
    pool_version: u64,
    captured: &Option<String>,
) -> bool {
    current
        .scopes
        .iter()
        .any(|declared| match &declared.subject {
            DeclaredSubject::Pool {
                pool_id: id,
                pool_version: version,
            } if id.as_str() == pool_id && *version == pool_version => {
                declared_model_covers(&declared.public_model, captured)
            }
            _ => false,
        })
}

fn record_attempt(
    document: &mut PolicyDocument,
    facts: &PolicyFacts,
    scopes: Vec<Scope>,
) -> Result<(), PolicyFault> {
    let Some(attempt) = facts.attempt.as_ref() else {
        return Err(PolicyFault::Malformed);
    };
    if document
        .attempts
        .iter()
        .any(|row| row.attempt_id == attempt.attempt_id)
    {
        return Ok(());
    }
    document.attempts.push(AdmittedAttempt {
        request_id: attempt.request_id,
        attempt_id: attempt.attempt_id,
        credential_id: attempt.credential_id.clone(),
        credential_version: attempt.credential_version,
        provider_id: attempt.provider_id.clone(),
        binding_id: attempt.binding_id.clone(),
        public_model: attempt.public_model.clone(),
        upstream_model: attempt.upstream_model.clone(),
        auth_id: attempt.auth_id.clone(),
        material_revision: attempt.material_revision.clone(),
        registration_epoch: attempt.registration_epoch,
        process_generation: facts.applied.process_generation,
        projection_revision: facts.applied.revision,
        projection_digest: facts.applied.digest,
        kind: attempt.kind,
        admitted_at: facts.now,
        retain_until: publication_until(facts)?,
        scopes,
    });
    Ok(())
}

fn publication_until(facts: &PolicyFacts) -> Result<DateTime<Utc>, PolicyFault> {
    facts
        .deadline_at
        .max(facts.now)
        .checked_add_signed(Duration::seconds(RESULT_PUBLICATION_GRACE_SECONDS))
        .ok_or(PolicyFault::Unavailable)
}

fn rejection_stamp(
    body: &ResultBody,
    admitted: &AdmittedAttempt,
    facts: &PolicyFacts,
) -> Option<Stamp> {
    match body.observation.as_ref() {
        Some(observation) if observation.fetched_at <= facts.now => Some(stamp_from(observation)),
        Some(_) => None,
        None => Some(Stamp {
            id: admitted.attempt_id.to_string(),
            fetched_at: facts.now,
        }),
    }
}

fn forget_attempt(document: &mut PolicyDocument, attempt_id: Uuid) {
    document.attempts.retain(|row| row.attempt_id != attempt_id);
}

fn stamp_from(observation: &ResultObservation) -> Stamp {
    Stamp {
        id: observation.id.clone(),
        fetched_at: observation.fetched_at,
    }
}

fn apply_evidence(
    document: &mut PolicyDocument,
    scopes: &[Scope],
    evidence: &Evidence,
    stamp: &Stamp,
    now: DateTime<Utc>,
) -> Result<(), PolicyFault> {
    for (window, observation) in &evidence.windows {
        for scope in scopes {
            match observation {
                Observation::Healthy => clear_if_newer(document, scope, *window, stamp),
                Observation::Ignore => {}
                Observation::Known(deadline) => {
                    upsert_known(
                        document,
                        scope.clone(),
                        *window,
                        *deadline,
                        evidence,
                        stamp,
                        now,
                    );
                }
                Observation::Unknown => {
                    upsert_unknown(document, scope.clone(), *window, evidence, stamp, now);
                }
            }
        }
    }
    Ok(())
}

fn stale_or_replay(existing: &Restriction, stamp: &Stamp) -> bool {
    existing.observation_id == stamp.id || stamp.fetched_at < existing.observed_at
}

fn note_stamp(existing: &mut Restriction, stamp: &Stamp, source: super::store::EvidenceSource) {
    existing.observed_at = stamp.fetched_at;
    existing.observation_id = stamp.id.clone();
    existing.source = source;
}

fn upsert_known(
    document: &mut PolicyDocument,
    scope: Scope,
    window: Window,
    deadline: DateTime<Utc>,
    evidence: &Evidence,
    stamp: &Stamp,
    now: DateTime<Utc>,
) {
    if deadline <= now {
        return;
    }
    if let Some(existing) = find_mut(document, &scope, window) {
        if stale_or_replay(existing, stamp) {
            return;
        }
        match existing.reset {
            Reset::Known { at } if deadline > at => {
                existing.reset = Reset::Known { at: deadline };
                existing.recovery = None;
                note_stamp(existing, stamp, evidence.source);
            }
            Reset::Known { .. } => note_stamp(existing, stamp, evidence.source),
            Reset::Unknown => {
                existing.reset = Reset::Known { at: deadline };
                existing.recovery = None;
                note_stamp(existing, stamp, evidence.source);
            }
        }
        return;
    }
    document.restrictions.push(Restriction {
        scope,
        window,
        reset: Reset::Known { at: deadline },
        observed_at: stamp.fetched_at,
        observation_id: stamp.id.clone(),
        source: evidence.source,
        recovery: None,
    });
}

fn upsert_unknown(
    document: &mut PolicyDocument,
    scope: Scope,
    window: Window,
    evidence: &Evidence,
    stamp: &Stamp,
    now: DateTime<Utc>,
) {
    if let Some(existing) = find_mut(document, &scope, window) {
        if stale_or_replay(existing, stamp) {
            return;
        }
        if let Reset::Known { at } = existing.reset {
            if at > now {
                note_stamp(existing, stamp, evidence.source);
                return;
            }
        }
        if existing.recovery.is_none() {
            existing.recovery = Some(UnknownRecovery::default());
        }
        existing.reset = Reset::Unknown;
        note_stamp(existing, stamp, evidence.source);
        return;
    }
    document.restrictions.push(Restriction {
        scope,
        window,
        reset: Reset::Unknown,
        observed_at: stamp.fetched_at,
        observation_id: stamp.id.clone(),
        source: evidence.source,
        recovery: Some(UnknownRecovery::default()),
    });
}

fn clear_if_newer(document: &mut PolicyDocument, scope: &Scope, window: Window, stamp: &Stamp) {
    let newer = {
        let Some(existing) = find_mut(document, scope, window) else {
            return;
        };
        existing.observation_id != stamp.id && stamp.fetched_at > existing.observed_at
    };
    if !newer {
        return;
    }
    let scope = scope.clone();
    document
        .restrictions
        .retain(|row| !(row.window == window && row.scope == scope));
}

fn find_mut<'a>(
    document: &'a mut PolicyDocument,
    scope: &'a Scope,
    window: Window,
) -> Option<&'a mut Restriction> {
    document
        .restrictions
        .iter_mut()
        .find(|row| row.window == window && &row.scope == scope)
}

fn sweep_expired(document: &mut PolicyDocument, now: DateTime<Utc>) {
    document.restrictions.retain(|row| match row.reset {
        Reset::Known { at } => at > now,
        Reset::Unknown => true,
    });
}

fn sweep_attempts(document: &mut PolicyDocument, now: DateTime<Utc>) {
    document
        .attempts
        .retain(|attempt| attempt.retain_until > now);
}

fn release_inflight(document: &mut PolicyDocument, attempt_id: Uuid) {
    for row in &mut document.restrictions {
        if let Some(recovery) = row.recovery.as_mut() {
            recovery
                .inflight
                .retain(|item| item.attempt_id != attempt_id);
        }
    }
}

fn applicable<'a>(document: &'a PolicyDocument, facts: &PolicyFacts) -> Vec<&'a Restriction> {
    document
        .restrictions
        .iter()
        .filter(|row| applies(row, facts))
        .collect()
}

pub(crate) fn applies(row: &Restriction, facts: &PolicyFacts) -> bool {
    let Some(attempt) = facts.attempt.as_ref() else {
        return false;
    };
    let Some(current) = facts.current.as_ref() else {
        return false;
    };
    restriction_matches(
        row,
        &current.credential_id,
        current.credential_version,
        &current.provider_id,
        &current.binding_id,
        &current.memberships,
        &attempt.public_model,
    )
}

/// Credential, pool, and model match shared by admission and explanation.
///
/// Model text is exact, matching admission. This does not require an attempt id.
pub(crate) fn restriction_matches(
    row: &Restriction,
    credential_id: &str,
    credential_version: u64,
    provider_id: &str,
    binding_id: &str,
    memberships: &[PoolMembership],
    public_model: &str,
) -> bool {
    if let Some(model) = &row.scope.public_model {
        if model != public_model {
            return false;
        }
    }
    match &row.scope.subject {
        Subject::Credential {
            credential_id: row_credential,
            credential_version: row_version,
            provider_id: row_provider,
            binding_id: row_binding,
        } => {
            row_credential == credential_id
                && *row_version == credential_version
                && row_provider == provider_id
                && row_binding == binding_id
        }
        Subject::Pool {
            pool_id,
            pool_version,
        } => memberships.iter().any(|member| {
            member.pool_id == *pool_id
                && member.pool_version == *pool_version
                && match &row.scope.public_model {
                    Some(model) => covers(member, model),
                    None => member.all_models,
                }
        }),
    }
}

fn latest_known(rows: &[&Restriction], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    rows.iter().fold(None, |latest, row| match row.reset {
        Reset::Known { at } if at > now => {
            Some(latest.map_or(at, |previous: DateTime<Utc>| previous.max(at)))
        }
        _ => latest,
    })
}

fn unknowns<'a>(rows: &[&'a Restriction]) -> Vec<&'a Restriction> {
    rows.iter()
        .copied()
        .filter(|row| matches!(row.reset, Reset::Unknown))
        .collect()
}

/// Current subject the caller already resolved. No request id and no attempt id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestrictionSubject {
    pub credential_id: String,
    pub credential_version: u64,
    pub provider_id: String,
    pub binding_id: String,
    pub memberships: Vec<PoolMembership>,
    pub public_model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetEvidence {
    Known { at: DateTime<Utc> },
    UnknownReset,
    Expired { at: DateTime<Utc> },
}

/// One matching restriction. Recovery leases are not copied and are not consumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestrictionEvidence {
    pub scope: Scope,
    pub window: Window,
    pub source: EvidenceSource,
    pub observed_at: DateTime<Utc>,
    pub observation_id: String,
    pub reset: ResetEvidence,
    /// Known future reset or unknown reset. Expired rows stay visible and do not block.
    /// An applicable unknown reset does not authorize a recovery trial.
    pub applicable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ScopedQuotaView {
    /// No row matches this subject and model. This is not unlimited quota.
    Unknown,
    Evidence(Vec<RestrictionEvidence>),
    /// The document could not be parsed. This is not an empty restriction list.
    Malformed,
}

/// Read-only evidence for one subject. Does not sweep or write the document.
pub(crate) fn scoped_restriction_evidence(
    loaded: Result<&PolicyDocument, PolicyFault>,
    subject: &RestrictionSubject,
    now: DateTime<Utc>,
) -> Result<ScopedQuotaView, PolicyFault> {
    let document = match loaded {
        Ok(document) => document,
        Err(PolicyFault::Malformed) => return Ok(ScopedQuotaView::Malformed),
        Err(fault) => return Err(fault),
    };
    let mut evidence = Vec::new();
    for row in &document.restrictions {
        if !restriction_matches(
            row,
            &subject.credential_id,
            subject.credential_version,
            &subject.provider_id,
            &subject.binding_id,
            &subject.memberships,
            &subject.public_model,
        ) {
            continue;
        }
        let (reset, applicable) = match row.reset {
            Reset::Known { at } if at > now => (ResetEvidence::Known { at }, true),
            Reset::Known { at } => (ResetEvidence::Expired { at }, false),
            Reset::Unknown => (ResetEvidence::UnknownReset, true),
        };
        evidence.push(RestrictionEvidence {
            scope: row.scope.clone(),
            window: row.window,
            source: row.source,
            observed_at: row.observed_at,
            observation_id: row.observation_id.clone(),
            reset,
            applicable,
        });
    }
    if evidence.is_empty() {
        Ok(ScopedQuotaView::Unknown)
    } else {
        Ok(ScopedQuotaView::Evidence(evidence))
    }
}

fn blocking_deadline(rows: &[&Restriction], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    latest_known(rows, now)
}

enum RowGate {
    Open,
    Wait(DateTime<Utc>),
    Budget,
}

fn admit_unknown(
    document: &mut PolicyDocument,
    facts: &PolicyFacts,
    opportunity: &OpportunityPolicy,
) -> Result<Decision, PolicyFault> {
    let Some(attempt) = facts.attempt.as_ref() else {
        return Ok(Decision::stop(Reason::Malformed));
    };
    let gap = Duration::seconds(i64::try_from(opportunity.gap_seconds).unwrap_or(i64::MAX));
    if inflight_attempt(document, facts, attempt.attempt_id) {
        let scopes = authorized_scopes(facts)?;
        record_attempt(document, facts, scopes)?;
        return Ok(Decision::allow(Reason::Eligible));
    }
    let leases = document
        .attempts
        .iter()
        .map(|row| (row.attempt_id, row.retain_until))
        .collect::<Vec<_>>();
    let gates = {
        let active = applicable(document, facts);
        unknowns(&active)
            .into_iter()
            .map(|row| {
                row_gate(
                    row,
                    &leases,
                    attempt.request_id,
                    facts.now,
                    gap,
                    opportunity,
                )
            })
            .collect::<Vec<_>>()
    };
    if gates.is_empty() {
        let scopes = authorized_scopes(facts)?;
        record_attempt(document, facts, scopes)?;
        return Ok(Decision::allow(Reason::Eligible));
    }
    let waiting = gates.iter().filter_map(|gate| match gate {
        RowGate::Wait(until) => Some(*until),
        _ => None,
    });
    if let Some(deadline) = waiting.max() {
        return Ok(Decision::skip(Reason::UnknownReset, Some(deadline)));
    }
    if gates.iter().any(|gate| matches!(gate, RowGate::Budget)) {
        return Ok(Decision::skip(Reason::UnknownReset, None));
    }
    consume_unknown(document, facts, attempt)?;
    let scopes = authorized_scopes(facts)?;
    record_attempt(document, facts, scopes)?;
    Ok(Decision::allow(Reason::Eligible))
}

fn sweep_unknown_leases(document: &mut PolicyDocument, now: DateTime<Utc>) {
    let live = document
        .attempts
        .iter()
        .filter(|row| row.retain_until > now)
        .map(|row| row.attempt_id)
        .collect::<Vec<_>>();
    for row in &mut document.restrictions {
        if let Some(recovery) = row.recovery.as_mut() {
            recovery
                .inflight
                .retain(|item| live.contains(&item.attempt_id));
        }
    }
}

fn inflight_attempt(document: &PolicyDocument, facts: &PolicyFacts, attempt_id: Uuid) -> bool {
    applicable(document, facts).into_iter().any(|row| {
        row.recovery.as_ref().is_some_and(|recovery| {
            recovery
                .inflight
                .iter()
                .any(|item| item.attempt_id == attempt_id)
        })
    })
}

fn row_gate(
    row: &Restriction,
    leases: &[(Uuid, DateTime<Utc>)],
    request_id: Uuid,
    now: DateTime<Utc>,
    gap: Duration,
    opportunity: &OpportunityPolicy,
) -> RowGate {
    let Some(recovery) = row.recovery.as_ref() else {
        return RowGate::Open;
    };
    let used = recovery
        .requests
        .iter()
        .find(|item| item.request_id == request_id)
        .map(|item| item.used)
        .unwrap_or(0);
    if used >= opportunity.max_per_request {
        return RowGate::Budget;
    }
    let mut wait = None;
    if recovery.inflight.len() as u32 >= opportunity.max_inflight {
        let until = recovery
            .inflight
            .iter()
            .filter_map(|item| {
                leases
                    .iter()
                    .find(|(attempt_id, _)| *attempt_id == item.attempt_id)
                    .map(|(_, until)| *until)
            })
            .min();
        if let Some(until) = until {
            if until > now {
                wait = Some(until);
            }
        }
    }
    if let Some(last) = recovery.last_admit_at {
        if let Some(until) = last.checked_add_signed(gap) {
            if until > now {
                wait = Some(wait.map_or(until, |previous| previous.max(until)));
            }
        }
    }
    match wait {
        Some(until) if until > now => RowGate::Wait(until),
        _ => RowGate::Open,
    }
}

fn consume_unknown(
    document: &mut PolicyDocument,
    facts: &PolicyFacts,
    attempt: &AttemptIdentity,
) -> Result<(), PolicyFault> {
    let keys: Vec<(Scope, Window)> = document
        .restrictions
        .iter()
        .filter(|row| applies(row, facts) && matches!(row.reset, Reset::Unknown))
        .map(|row| (row.scope.clone(), row.window))
        .collect();
    for (scope, window) in keys {
        let Some(row) = find_mut(document, &scope, window) else {
            continue;
        };
        let recovery = row.recovery.get_or_insert_with(UnknownRecovery::default);
        if recovery
            .inflight
            .iter()
            .any(|item| item.attempt_id == attempt.attempt_id)
        {
            continue;
        }
        if !remember_request(recovery, attempt.request_id) {
            return Err(PolicyFault::Unavailable);
        }
        recovery.last_admit_at = Some(facts.now);
        recovery.inflight.push(InflightAdmit {
            request_id: attempt.request_id,
            attempt_id: attempt.attempt_id,
            at: facts.now,
        });
    }
    Ok(())
}
