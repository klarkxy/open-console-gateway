//! Synchronous CPA policy callback for precise Plan restrictions.
//!
//! The host mounts this service later. It admits or classifies one attempt; it
//! does not select a provider, retry, or send. Authoritative rows live in the
//! existing SQLite `settings` table through [`store::transact_settings`].

mod auth;
mod decide;
mod evidence;
mod store;
mod wire;

pub(crate) use decide::{
    RESULT_PUBLICATION_GRACE_SECONDS, ResetEvidence, RestrictionEvidence, RestrictionSubject,
    ScopedQuotaView, restriction_matches, result_consumes_attempt, scoped_restriction_evidence,
};

pub use auth::authorize_token;
pub use store::{
    AdmittedAttempt, EvidenceSource, PolicyDocument, PolicyFault, PolicyStore, Reset, Restriction,
    SETTINGS_KEY, Scope, SendKind, Subject, Window, read_settings, read_transaction,
    transact_settings,
};
pub use wire::{
    Action, AppliedProjection, AttemptFields, AttemptIdentity, CurrentCredential, CurrentFacts,
    Decision, DeclaredScope, DeclaredSubject, ErrorCode, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES,
    OfficialQuotaObservation, OpportunityPolicy, Outcome, ParseFault, ParsedRequest, PolicyFacts,
    PolicyReply, PoolMembership, QuotaApply, ReadyReport, Reason, RequestKind, ResultBody,
    parse_request,
};

use ocg_domain::ids::OPENCODE_PROVIDER_ID;
use parking_lot::Mutex;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use store::PolicyStore as Store;
use wire::{MAX_EVIDENCE_BODY_BYTES, MAX_TOKEN_BYTES, encode_decision, encode_ready};

/// In-process gate so one result transaction commits before the next admission.
///
/// The gate covers only the settings transaction. It is not held across the
/// CPA callback that invoked this service, and the store must not call back
/// into the service or perform network I/O.
pub struct PolicyService<S: Store> {
    token: String,
    opportunity: OpportunityPolicy,
    gate: Mutex<()>,
    store: S,
}

impl<S: Store> PolicyService<S> {
    pub fn new(token: &str, opportunity: OpportunityPolicy, store: S) -> Result<Self, PolicyFault> {
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
            return Err(PolicyFault::Malformed);
        }
        opportunity.check()?;
        Ok(Self {
            token: token.to_string(),
            opportunity,
            gate: Mutex::new(()),
            store,
        })
    }

    /// Authenticate and answer one v1 body.
    ///
    /// `current.revalidate` runs inside the settings transaction. It must
    /// re-read credential version, binding, grants, deletion, and explicit
    /// pool membership on that transaction, and must not do network I/O.
    pub fn handle(
        &self,
        provided_token: &str,
        body: &[u8],
        current: &mut dyn CurrentFacts,
    ) -> PolicyReply {
        let authorized = authorize_token(self.token.as_bytes(), provided_token.as_bytes());
        if !authorized {
            return encode_decision(
                401,
                Decision::stop(Reason::Unauthorized),
                wire::ReplyClass::Decision,
            );
        }
        if body.len() > MAX_REQUEST_BYTES {
            return encode_decision(
                413,
                Decision::stop(Reason::Oversize),
                wire::ReplyClass::Decision,
            );
        }
        let request = match wire::parse_request(body) {
            Ok(request) => request,
            Err(wire::ParseFault::Oversize) => {
                return encode_decision(
                    413,
                    Decision::stop(Reason::Oversize),
                    wire::ReplyClass::Decision,
                );
            }
            Err(wire::ParseFault::Malformed) => {
                return encode_decision(
                    200,
                    Decision::stop(Reason::Malformed),
                    wire::ReplyClass::Decision,
                );
            }
        };
        let _gate = self.gate.lock();
        match &request.kind {
            wire::RequestKind::Ready => self.ready(&request, current),
            wire::RequestKind::Admit => self.admit(&request, current),
            wire::RequestKind::Result(result) => self.result(&request, result, current, body),
        }
    }

    /// Apply one official Go usage snapshot. Inference text and GOAT
    /// calibration are not accepted here.
    pub fn apply_official_quota(
        &self,
        observation: &OfficialQuotaObservation,
        current: &mut dyn CurrentFacts,
    ) -> Result<QuotaApply, PolicyFault> {
        validate_official_observation(observation)?;
        let _gate = self.gate.lock();
        let mut outcome = QuotaApply::Stale;
        self.store.update(&mut |tx, document| {
            outcome = official_quota_mutation(tx, document, observation, current)?;
            Ok(())
        })?;
        Ok(outcome)
    }

    fn ready(&self, request: &ParsedRequest, current: &mut dyn CurrentFacts) -> PolicyReply {
        let mut report = None;
        let read = self.store.read(&mut |tx, _document| {
            let facts = current.revalidate(tx)?;
            if facts.invalid() {
                report = Some(unavailable_ready());
                return Ok(());
            }
            if let Some(reason) = decide::projection_fence(request, &facts) {
                report = Some(facts_ready(&facts, false, reason, false));
                return Ok(());
            }
            report = Some(facts_ready(&facts, true, Reason::Ready, false));
            Ok(())
        });
        match read {
            Ok(()) => encode_ready(report.unwrap_or_else(unavailable_ready)),
            Err(_) => encode_ready(unavailable_ready()),
        }
    }

    fn admit(&self, request: &ParsedRequest, current: &mut dyn CurrentFacts) -> PolicyReply {
        let mut decision = Decision::unavailable();
        let updated = self.store.update(&mut |tx, document| {
            let facts = current.revalidate(tx)?;
            if facts.invalid() {
                decision = Decision::stop(Reason::Malformed);
                return Ok(());
            }
            decision = decide::admit(document, request, &facts, &self.opportunity)?;
            if decision.action == wire::Action::Allow {
                if let Some(identity) = facts.attempt.as_ref() {
                    decision.endpoint_pins =
                        current.freeze_admitted_attempt(tx, identity, facts.now)?;
                }
            }
            Ok(())
        });
        let mut decision = fold_store(updated, decision);
        if decision.action != wire::Action::Allow {
            decision.endpoint_pins = None;
        }
        encode_decision(200, decision, wire::ReplyClass::Decision)
    }

    fn result(
        &self,
        request: &ParsedRequest,
        result: &wire::ResultBody,
        current: &mut dyn CurrentFacts,
        raw: &[u8],
    ) -> PolicyReply {
        let raw_body = std::str::from_utf8(raw).unwrap_or("");
        let mut decision = Decision::unavailable();
        let updated = self.store.update(&mut |tx, document| {
            let mut facts = current.revalidate(tx)?;
            if facts.invalid() {
                decision = Decision::stop(Reason::Malformed);
                return Ok(());
            }
            if let Some(pin) = result.endpoint_pin.as_ref() {
                if let Some(identity) = facts.attempt.as_ref() {
                    if let Some(frozen) = current.frozen_endpoint_pins(identity.attempt_id) {
                        let member = frozen
                            .as_ref()
                            .is_some_and(|allowed| allowed.iter().any(|item| item == pin));
                        if !member {
                            decision = Decision::stop(Reason::IdentityFence);
                            return Ok(());
                        }
                    }
                }
            }
            if !current.retains_quota_restriction_authority() {
                if let Some(live) = facts.current.as_mut() {
                    live.granted = false;
                }
            }
            decision = decide::result(document, request, &facts, result)?;
            if decide::result_consumes_attempt(decision.reason) {
                if let Some(identity) = facts.attempt.as_ref() {
                    current.record_admitted_result(tx, &decision, identity, result, raw_body)?;
                }
            }
            Ok(())
        });
        encode_decision(
            200,
            fold_store(updated, decision),
            wire::ReplyClass::Decision,
        )
    }
}

fn validate_official_observation(
    observation: &OfficialQuotaObservation,
) -> Result<(), PolicyFault> {
    if observation.observation_id.is_empty()
        || observation.observation_id.len() > 128
        || observation.body.len() > MAX_EVIDENCE_BODY_BYTES
        || observation.provider_id.is_empty()
    {
        return Err(PolicyFault::Malformed);
    }
    Ok(())
}

fn official_quota_mutation(
    tx: &Transaction<'_>,
    document: &mut PolicyDocument,
    observation: &OfficialQuotaObservation,
    current: &mut dyn CurrentFacts,
) -> Result<QuotaApply, PolicyFault> {
    let facts = current.revalidate(tx)?;
    if facts.invalid() {
        return Err(PolicyFault::Malformed);
    }
    let Some(live) = facts.current.as_ref() else {
        return Err(PolicyFault::Malformed);
    };
    if live.deleted
        || live.rebound
        || live.provider_id != observation.provider_id
        || observation.provider_id != OPENCODE_PROVIDER_ID
    {
        return Ok(QuotaApply::Stale);
    }
    if observation.fetched_at > facts.now {
        return Err(PolicyFault::Malformed);
    }
    let parsed = evidence::parse_official_usage(&observation.body, observation.fetched_at)
        .map_err(|_| PolicyFault::Malformed)?;
    let observed = evidence::from_official_usage(&parsed, facts.now);
    decide::apply_quota(
        document,
        &facts,
        &observation.observation_id,
        observation.fetched_at,
        &observed,
    )?;
    Ok(QuotaApply::Applied)
}

/// Apply one official observation on the caller's connection.
///
/// The caller holds the serialized connection and has no transaction open.
/// `PolicyService::apply_official_quota` uses the same mutation. This helper
/// does not open a second connection.
pub(crate) fn apply_official_quota_on_connection(
    conn: &Connection,
    observation: &OfficialQuotaObservation,
    current: &mut dyn CurrentFacts,
) -> Result<QuotaApply, PolicyFault> {
    validate_official_observation(observation)?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(|_| PolicyFault::Unavailable)?;
    let mut document = store::read_in_transaction(&tx, SETTINGS_KEY)?;
    let outcome = official_quota_mutation(&tx, &mut document, observation, current)?;
    let json = document.to_json()?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![SETTINGS_KEY, json],
    )
    .map_err(|_| PolicyFault::Unavailable)?;
    tx.commit().map_err(|_| PolicyFault::Unavailable)?;
    Ok(outcome)
}

fn fold_store(updated: Result<(), PolicyFault>, decision: Decision) -> Decision {
    match updated {
        Ok(()) => decision,
        Err(_) => Decision::unavailable(),
    }
}

fn facts_ready(facts: &PolicyFacts, ready: bool, reason: Reason, unavailable: bool) -> ReadyReport {
    ReadyReport {
        policy_ready: ready,
        process_generation: facts.applied.process_generation,
        projection_revision: facts.applied.revision,
        projection_digest: facts.applied.digest,
        reason,
        unavailable,
    }
}

fn unavailable_ready() -> ReadyReport {
    ReadyReport {
        policy_ready: false,
        process_generation: 0,
        projection_revision: 0,
        projection_digest: [0; 32],
        reason: Reason::Unavailable,
        unavailable: true,
    }
}

#[cfg(test)]
mod tests;
