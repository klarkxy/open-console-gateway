//! Process-local admission for observed restrictions, separate from quota rows.
//! Only real client requests probe. Pending resources admit one probe at a time;
//! healthy resources remain concurrent. Dropping a request always releases it.
use super::failure::{FailureDecision, FailureFacts, RetryHint, Scope};
use super::policy::{
    EffectivePolicyRule, EffectivePolicySnapshot, PolicyBackoff, PolicyDecision, PolicyInput,
    PolicySource, PolicySourceKind, RestrictionScope, evaluate_rules,
};
use crate::models::UsageWindowKind;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod context;
pub(crate) use context::{ResourceSet, restriction_endpoint_identity};

const MAX_TRACKED_RESOURCES: usize = 4096;
const INITIAL_PROBE_SECS: u64 = 30;
const MAX_PROBE_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum ResourceKind {
    /// Independent upstream not-before for a persistent per-Key quota episode.
    CredentialRetry,
    EndpointModel,
    Credits,
    FiveHours,
    Week,
    Month,
    FreeEgress,
    /// Local policy: current authorized binding / credential generation.
    PolicyCredential,
    /// Local policy: credential generation plus actual send endpoint/route/protocol/model.
    PolicyCredentialModel,
}

impl ResourceKind {
    fn is_policy(self) -> bool {
        matches!(self, Self::PolicyCredential | Self::PolicyCredentialModel)
    }
}
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct ResourceKey {
    kind: ResourceKind,
    generation: [u8; 32],
}

pub(crate) struct RecoveryRuntime {
    inner: Mutex<Inner>,
}

impl Default for RecoveryRuntime {
    fn default() -> Self {
        Self::with_snapshot(EffectivePolicySnapshot::builtin())
    }
}

impl RecoveryRuntime {
    pub(crate) fn with_snapshot(snapshot: EffectivePolicySnapshot) -> Self {
        Self {
            inner: Mutex::new(Inner {
                sequence: 0,
                restriction_seq: 0,
                slots: HashMap::new(),
                policy_snapshot: snapshot,
            }),
        }
    }
}
struct Inner {
    sequence: u64,
    restriction_seq: u64,
    slots: HashMap<ResourceKey, Slot>,
    policy_snapshot: EffectivePolicySnapshot,
}
#[derive(Default)]
struct Slot {
    owners: Vec<String>,
    owner_generation: [u8; 32],
    active: usize,
    revision: u64,
    fence: u64,
    awaiting_recovery: bool,
    upstream_not_before: Option<RetryHint>,
    next_probe: Option<Instant>,
    probe_owner: Option<u64>,
    failures: u32,
    policy: HashMap<PolicySource, PolicyRestriction>,
    retired_policy: HashMap<PolicySource, u64>,
    destination_id: String,
    credential_id: String,
    upstream_model: Option<String>,
}
#[derive(Default)]
struct PolicyRestriction {
    next_probe: Option<Instant>,
    failures: u32,
    observe_fence: u64,
    opaque_id: String,
    backoff: Option<PolicyBackoff>,
    /// Episode version. A later observation or ABA recreate must not be
    /// cleared by a probe that leased an older revision of this source.
    revision: u64,
}
impl Slot {
    fn restricted(&self) -> bool {
        self.awaiting_recovery
            || self.upstream_not_before.is_some()
            || self.next_probe.is_some()
            || self.policy.values().any(|row| row.next_probe.is_some())
    }
    fn clear(&mut self, fence: u64) {
        self.awaiting_recovery = false;
        self.upstream_not_before = None;
        self.next_probe = None;
        self.probe_owner = None;
        self.failures = 0;
        self.policy.clear();
        self.retired_policy.clear();
        self.fence = self.fence.max(fence);
        self.revision = self.revision.wrapping_add(1);
    }
    fn blocks(&self, kind: ResourceKind, wall: DateTime<Utc>, mono: Instant) -> Option<WaitState> {
        let upstream_wait = match self.upstream_not_before {
            Some(RetryHint::Unbounded) => true,
            Some(RetryHint::Until(at)) => at > wall,
            None => false,
        };
        let local_wait = self.next_probe.is_some_and(|at| at > mono);
        let policy_wait = kind.is_policy()
            && self
                .policy
                .values()
                .any(|row| row.next_probe.is_some_and(|at| at > mono));
        let probing = self.probe_owner.is_some();
        if !(upstream_wait || local_wait || policy_wait || probing) {
            return None;
        }
        let sources = if kind.is_policy() {
            self.policy
                .iter()
                .map(|(source, row)| {
                    let waiting = row.next_probe.is_some_and(|at| at > mono);
                    WaitSource {
                        owner: source.owner.clone(),
                        rule_id: source.rule_id.clone(),
                        rule_generation: source.rule_generation,
                        next_probe_in_seconds: row.next_probe.filter(|at| *at > mono).map(|at| {
                            at.saturating_duration_since(mono)
                                .as_secs()
                                .saturating_add(1)
                        }),
                        ready: !waiting && !probing,
                        probing,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let next_probe_in_seconds = self
            .next_probe
            .filter(|at| *at > mono)
            .or_else(|| {
                self.policy
                    .values()
                    .filter_map(|row| row.next_probe.filter(|at| *at > mono))
                    .max()
            })
            .map(|at| {
                at.saturating_duration_since(mono)
                    .as_secs()
                    .saturating_add(1)
            });
        Some(WaitState {
            reason: if kind.is_policy() && !upstream_wait && !local_wait {
                "local_policy"
            } else {
                "resource_waiting_for_recovery"
            },
            upstream_not_before: self.upstream_not_before,
            next_probe_in_seconds,
            probe_in_flight: probing,
            sources,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RestrictionRecord {
    pub id: String,
    pub rule_id: String,
    pub rule_generation: u64,
    pub source: PolicySourceKind,
    pub credential_id: String,
    pub destination_id: String,
    pub scope: RestrictionScope,
    pub upstream_model: Option<String>,
    pub state: &'static str,
    pub next_probe_in_seconds: Option<u64>,
    pub probe_in_flight: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WaitSource {
    pub owner: String,
    pub rule_id: String,
    pub rule_generation: u64,
    pub next_probe_in_seconds: Option<u64>,
    pub ready: bool,
    pub probing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WaitState {
    pub reason: &'static str,
    pub upstream_not_before: Option<RetryHint>,
    pub next_probe_in_seconds: Option<u64>,
    pub probe_in_flight: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<WaitSource>,
}
impl WaitState {
    fn capacity() -> Self {
        Self {
            reason: "recovery_capacity",
            upstream_not_before: None,
            next_probe_in_seconds: None,
            probe_in_flight: false,
            sources: Vec::new(),
        }
    }
    pub(crate) fn is_local_policy(&self) -> bool {
        self.reason == "local_policy"
    }
    pub(crate) fn is_capacity(&self) -> bool {
        self.reason == "recovery_capacity"
    }
    pub(crate) fn skip_stage(&self) -> &'static str {
        match self.reason {
            "local_policy" => "local_policy_skip",
            "recovery_capacity" => "recovery_capacity",
            _ => "resource_wait",
        }
    }
}
struct LeasedPolicy {
    source: PolicySource,
    opaque_id: String,
    revision: u64,
}

struct Claim {
    key: ResourceKey,
    revision: u64,
    probe: bool,
    leased_policy: Vec<LeasedPolicy>,
}

/// Non-owning observation token. The attempt's permit keeps the slot alive;
/// clones let streaming quota observations honor the same operator-reset fence.
#[derive(Clone)]
pub(crate) struct RecoveryObservation {
    runtime: Arc<RecoveryRuntime>,
    key: ResourceKey,
    ticket: u64,
}
impl RecoveryObservation {
    pub(crate) fn is_current(&self) -> bool {
        self.runtime
            .inner
            .lock()
            .slots
            .get(&self.key)
            .is_some_and(|slot| self.ticket >= slot.fence)
    }
}

pub(crate) struct RecoveryPermit {
    runtime: Arc<RecoveryRuntime>,
    resources: ResourceSet,
    claims: Vec<Claim>,
    ticket: u64,
    cancelled_probe_at: Instant,
    observed: Vec<ResourceKey>,
    lease_started: Instant,
    captured_policy: Vec<EffectivePolicyRule>,
}

impl RecoveryRuntime {
    pub(crate) fn acquire(
        self: &Arc<Self>,
        resources: ResourceSet,
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Result<RecoveryPermit, WaitState> {
        let keys = resources.keys();
        let mut inner = self.inner.lock();
        // A changed credential generation cannot inherit old credit state.
        // Retire inactive obsolete quota generations without forgetting another
        // model's restriction in the *current* generation or any endpoint wait.
        inner.slots.retain(|key, slot| {
            // Until deadlines (Key retry and independent policy Retry-After)
            // have no probe to clear them. Expire them so elapsed waits cannot
            // pin single-flight or occupy capacity.
            expire_until_deadline(slot, wall);
            slot_kept_after_cleanup(key, slot, &resources, wall)
        });
        let new_count = keys
            .iter()
            .filter(|key| !inner.slots.contains_key(*key))
            .count();
        if inner.slots.len() + new_count > MAX_TRACKED_RESOURCES {
            return Err(WaitState::capacity());
        }
        for key in &keys {
            if !resources.enforces(key) {
                continue;
            }
            if let Some(slot) = inner.slots.get(key)
                && let Some(wait) = slot.blocks(key.kind, wall, mono)
            {
                return Err(wait);
            }
        }
        inner.sequence = inner
            .sequence
            .checked_add(1)
            .ok_or_else(WaitState::capacity)?;
        let ticket = inner.sequence;
        let claims = keys
            .into_iter()
            .map(|key| {
                let slot = inner.slots.entry(key.clone()).or_default();
                slot.owner_generation = resources.owner_generation(&key);
                for owner in resources.owners(&key) {
                    if !slot.owners.contains(owner) {
                        slot.owners.push(owner.clone());
                    }
                }
                slot.active += 1;
                slot.destination_id = resources.destination_id.clone();
                slot.credential_id = resources.credential_id.clone();
                slot.upstream_model = resources.upstream_model.clone();
                let probe = key.kind != ResourceKind::CredentialRetry
                    && resources.enforces(&key)
                    && slot.restricted();
                if probe {
                    slot.probe_owner = Some(ticket);
                }
                let leased_policy = if key.kind.is_policy() {
                    slot.policy
                        .iter()
                        .map(|(source, row)| LeasedPolicy {
                            source: source.clone(),
                            opaque_id: row.opaque_id.clone(),
                            revision: row.revision,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Claim {
                    key,
                    revision: slot.revision,
                    probe,
                    leased_policy,
                }
            })
            .collect();
        let captured_policy = inner
            .policy_snapshot
            .effective_for(&resources.destination_id);
        Ok(RecoveryPermit {
            runtime: self.clone(),
            resources,
            claims,
            ticket,
            cancelled_probe_at: mono + Duration::from_secs(INITIAL_PROBE_SECS),
            observed: Vec::new(),
            lease_started: Instant::now(),
            captured_policy,
        })
    }

    /// Read the current credential generation's temporary wait without claiming
    /// a probe or extending it. The executor uses this after exhausting fallbacks.
    pub(crate) fn credential_retry_until(
        &self,
        resources: &ResourceSet,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        self.inner
            .lock()
            .slots
            .get(&resources.key(ResourceKind::CredentialRetry))
            .and_then(|slot| match slot.upstream_not_before {
                Some(RetryHint::Until(until)) if until > now => Some(until),
                Some(RetryHint::Unbounded) => Some(DateTime::<Utc>::MAX_UTC),
                _ => None,
            })
    }

    /// Process-wide anonymous Free egress wait (Zen Free shared IP scope).
    /// Combines a wall-clock Retry-After / temporary 429 deadline with any
    /// remaining local reprobe so "all waiting" can return 429, not 503.
    pub(crate) fn free_egress_retry_until(
        &self,
        now: DateTime<Utc>,
        mono: Instant,
    ) -> Option<DateTime<Utc>> {
        let key = ResourceKey {
            kind: ResourceKind::FreeEgress,
            generation: [0; 32],
        };
        let inner = self.inner.lock();
        let slot = inner.slots.get(&key)?;
        let upstream = match slot.upstream_not_before {
            Some(RetryHint::Until(until)) if until > now => Some(until),
            Some(RetryHint::Unbounded) => Some(DateTime::<Utc>::MAX_UTC),
            _ => None,
        };
        let probe = slot.next_probe.filter(|at| *at > mono).and_then(|at| {
            let secs = i64::try_from(at.saturating_duration_since(mono).as_secs()).ok()?;
            now.checked_add_signed(chrono::Duration::seconds(secs.saturating_add(1)))
        });
        [upstream, probe].into_iter().flatten().max()
    }

    /// Explicit operator reset. A fence prevents old in-flight replies from
    /// recreating the state the operator just cleared. Shared members reset the
    /// same resource. No automatic reset accompanies ordinary success elsewhere.
    pub(crate) fn reset_account(&self, account_id: &str) {
        let mut inner = self.inner.lock();
        let fence = inner.sequence.saturating_add(1);
        for (_, slot) in inner.slots.iter_mut().filter(|(key, slot)| {
            key.kind != ResourceKind::FreeEgress && slot.owners.iter().any(|id| id == account_id)
        }) {
            slot.clear(fence);
        }
        inner
            .slots
            .retain(|_, slot| slot.active != 0 || slot.restricted());
    }

    /// Preview admission without claiming a lease or creating slots.
    /// Capacity and expired Until deadlines (Key retry and independent policy
    /// Retry-After) follow the same reclaim rules as [`Self::acquire`] so a
    /// full table cannot permanently skip cleanup.
    pub(crate) fn inspect_admission(
        &self,
        resources: &ResourceSet,
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Result<(), WaitState> {
        let inner = self.inner.lock();
        let keys = resources.keys();
        for key in &keys {
            if !resources.enforces(key) {
                continue;
            }
            if let Some(slot) = inner.slots.get(key) {
                if !slot_kept_after_cleanup(key, slot, resources, wall) {
                    continue;
                }
                if let Some(wait) = slot.blocks(key.kind, wall, mono) {
                    return Err(wait);
                }
            }
        }
        let projected = inner
            .slots
            .iter()
            .filter(|(key, slot)| slot_kept_after_cleanup(key, slot, resources, wall))
            .count();
        let new_count = keys
            .iter()
            .filter(|key| {
                !inner
                    .slots
                    .get(*key)
                    .is_some_and(|slot| slot_kept_after_cleanup(key, slot, resources, wall))
            })
            .count();
        if projected + new_count > MAX_TRACKED_RESOURCES {
            return Err(WaitState::capacity());
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn evaluate_policy(
        &self,
        destination_id: &str,
        input: &PolicyInput,
    ) -> Vec<PolicyDecision> {
        let inner = self.inner.lock();
        evaluate_rules(&inner.policy_snapshot.effective_for(destination_id), input)
    }

    pub(crate) fn policy_snapshot(&self) -> EffectivePolicySnapshot {
        self.inner.lock().policy_snapshot.clone()
    }

    /// Replace the live rule set. Sources no longer effective for a slot's
    /// destination are retired with a source-local observe fence. Unrelated
    /// destinations, Retry-After, and other sources stay.
    pub(crate) fn install_snapshot(&self, snapshot: EffectivePolicySnapshot) {
        let mut inner = self.inner.lock();
        inner.policy_snapshot = snapshot;
        let compiled = inner.policy_snapshot.clone();
        let fence = inner.sequence.saturating_add(1);
        for slot in inner.slots.values_mut() {
            let dest = slot.destination_id.clone();
            let stale: Vec<_> = slot
                .policy
                .keys()
                .filter(|source| !compiled.is_live(source, &dest))
                .cloned()
                .collect();
            for source in stale {
                retire_source(slot, &source, fence);
            }
        }
        retain_live_slots(&mut inner);
    }

    /// Precise local revoke of one restriction id. Does not declare health,
    /// send, or clear other sources / Retry-After.
    pub(crate) fn clear_restriction(&self, restriction_id: &str) {
        let mut inner = self.inner.lock();
        let fence = inner.sequence.saturating_add(1);
        for slot in inner.slots.values_mut() {
            let stale: Vec<_> = slot
                .policy
                .iter()
                .filter(|(_, row)| row.opaque_id == restriction_id)
                .map(|(source, _)| source.clone())
                .collect();
            for source in stale {
                retire_source(slot, &source, fence);
            }
        }
        retain_live_slots(&mut inner);
    }

    /// Retire one source across every resource (configuration deleted the rule).
    #[cfg(test)]
    pub(crate) fn clear_source(&self, source: &PolicySource) {
        let mut inner = self.inner.lock();
        let fence = inner.sequence.saturating_add(1);
        clear_source_locked(&mut inner, source, fence);
        retain_live_slots(&mut inner);
    }

    #[cfg(test)]
    pub(crate) fn tracked_slot_count(&self) -> usize {
        self.inner.lock().slots.len()
    }

    pub(crate) fn list_restrictions(&self, mono: Instant) -> Vec<RestrictionRecord> {
        let inner = self.inner.lock();
        let mut rows = Vec::new();
        for (key, slot) in &inner.slots {
            if !key.kind.is_policy() {
                continue;
            }
            let scope = match key.kind {
                ResourceKind::PolicyCredential => RestrictionScope::Credential,
                _ => RestrictionScope::CredentialModel,
            };
            let probing = slot.probe_owner.is_some();
            for (source, restriction) in &slot.policy {
                let waiting = restriction.next_probe.is_some_and(|at| at > mono);
                let state = if probing {
                    "probing"
                } else if waiting {
                    "waiting"
                } else {
                    "ready"
                };
                rows.push(RestrictionRecord {
                    id: restriction.opaque_id.clone(),
                    rule_id: source.rule_id.clone(),
                    rule_generation: source.rule_generation,
                    source: source.kind(),
                    credential_id: slot.credential_id.clone(),
                    destination_id: slot.destination_id.clone(),
                    scope,
                    upstream_model: if scope == RestrictionScope::Credential {
                        None
                    } else {
                        slot.upstream_model.clone()
                    },
                    state,
                    next_probe_in_seconds: restriction.next_probe.filter(|at| *at > mono).map(
                        |at| {
                            at.saturating_duration_since(mono)
                                .as_secs()
                                .saturating_add(1)
                        },
                    ),
                    probe_in_flight: probing,
                });
            }
        }
        rows
    }
}

fn expire_until_deadline(slot: &mut Slot, wall: DateTime<Utc>) {
    if matches!(slot.upstream_not_before, Some(RetryHint::Until(at)) if at <= wall) {
        slot.upstream_not_before = None;
    }
}

fn deadline_is_live(hint: Option<RetryHint>, wall: DateTime<Utc>) -> bool {
    match hint {
        Some(RetryHint::Unbounded) => true,
        Some(RetryHint::Until(at)) => at > wall,
        None => false,
    }
}

fn projected_restricted(slot: &Slot, wall: DateTime<Utc>) -> bool {
    slot.awaiting_recovery
        || deadline_is_live(slot.upstream_not_before, wall)
        || slot.next_probe.is_some()
        || slot.policy.values().any(|row| row.next_probe.is_some())
}

fn slot_kept_after_cleanup(
    key: &ResourceKey,
    slot: &Slot,
    resources: &ResourceSet,
    wall: DateTime<Utc>,
) -> bool {
    if slot.active == 0
        && !projected_restricted(slot, wall)
        && !matches!(
            key.kind,
            ResourceKind::EndpointModel | ResourceKind::FreeEgress
        )
    {
        return false;
    }
    matches!(
        key.kind,
        ResourceKind::EndpointModel | ResourceKind::FreeEgress
    ) || slot.active != 0
        || slot.owners.as_slice() != resources.owners(key)
        || slot.owner_generation == resources.owner_generation(key)
}

fn policy_backoff_secs(backoff: PolicyBackoff, failures: u32, jitter_seed: u8) -> u64 {
    let exp = failures.saturating_sub(1).min(63);
    let factor = 1u64.checked_shl(exp).unwrap_or(u64::MAX);
    let base = backoff
        .initial_secs
        .saturating_mul(factor)
        .min(backoff.max_secs);
    let jitter = u64::from(jitter_seed) % (base / 10 + 1);
    (base + jitter).min(backoff.max_secs)
}

fn retire_source(slot: &mut Slot, source: &PolicySource, fence: u64) {
    slot.policy.remove(source);
    let prior = slot.retired_policy.get(source).copied().unwrap_or(0);
    slot.retired_policy.insert(source.clone(), prior.max(fence));
    if !slot.restricted() {
        slot.probe_owner = None;
    }
}

#[cfg(test)]
fn clear_source_locked(inner: &mut Inner, source: &PolicySource, fence: u64) {
    for slot in inner.slots.values_mut() {
        retire_source(slot, source, fence);
    }
}

fn retain_live_slots(inner: &mut Inner) {
    inner
        .slots
        .retain(|_, slot| slot.active != 0 || slot.restricted());
}

impl RecoveryPermit {
    pub(crate) fn quota_observation(&self) -> RecoveryObservation {
        RecoveryObservation {
            runtime: self.runtime.clone(),
            key: self.resources.key(ResourceKind::CredentialRetry),
            ticket: self.ticket,
        }
    }

    pub(crate) fn permits_observation(&self, facts: &FailureFacts) -> bool {
        self.runtime
            .inner
            .lock()
            .slots
            .get(&self.resources.for_facts(facts))
            .is_some_and(|slot| self.ticket >= slot.fence)
    }

    pub(crate) fn permits_policy(&self, decision: &PolicyDecision) -> bool {
        let Some(key) = self.resources.policy_key(decision.scope) else {
            return false;
        };
        let inner = self.runtime.inner.lock();
        if !inner
            .policy_snapshot
            .is_live(&decision.source, &self.resources.destination_id)
        {
            return false;
        }
        let Some(slot) = inner.slots.get(&key) else {
            return false;
        };
        let retired = slot
            .retired_policy
            .get(&decision.source)
            .copied()
            .unwrap_or(0);
        self.ticket >= slot.fence && self.ticket >= retired
    }

    pub(crate) fn same_generation(&self, resources: &ResourceSet) -> bool {
        self.resources.same_generation(resources)
    }

    pub(crate) fn same_policy_identity(&self, resources: &ResourceSet) -> bool {
        self.resources.same_policy_identity(resources)
    }

    #[allow(dead_code)]
    pub(crate) fn captured_policy(&self) -> &[EffectivePolicyRule] {
        &self.captured_policy
    }

    pub(crate) fn evaluate_captured(&self, input: &PolicyInput) -> Vec<PolicyDecision> {
        evaluate_rules(&self.captured_policy, input)
    }

    pub(crate) fn observe_failure(
        &mut self,
        facts: &FailureFacts,
        decision: FailureDecision,
        mono: Instant,
    ) {
        self.observe_key(self.resources.for_facts(facts), decision, mono);
    }

    /// Persistent quota recovery owns the episode and probe. Preserve a separate
    /// upstream Retry-After for this Key without imposing a second probe or
    /// extending that credential's restriction to its declared quota pool.
    pub(crate) fn observe_credential_retry(&mut self, hint: Option<RetryHint>, mono: Instant) {
        self.observe_key(
            self.resources.key(ResourceKind::CredentialRetry),
            FailureDecision {
                persist_reset: None,
                wait_for_recovery: false,
                retry_not_before: hint,
                exhaust_free: false,
            },
            mono,
        );
    }

    pub(crate) fn observe_policy(&mut self, decision: &PolicyDecision, mono: Instant) {
        let Some(key) = self.resources.policy_key(decision.scope) else {
            return;
        };
        let dest = self.resources.destination_id.clone();
        let probing = self
            .claims
            .iter()
            .any(|claim| claim.key == key && claim.probe);
        let mut inner = self.runtime.inner.lock();
        if !inner.policy_snapshot.is_live(&decision.source, &dest) {
            return;
        }
        let next_seq = inner.restriction_seq.saturating_add(1);
        let opaque_id = format!("tp-{next_seq}");
        let inserting = {
            let Some(slot) = inner.slots.get_mut(&key) else {
                return;
            };
            let retired = slot
                .retired_policy
                .get(&decision.source)
                .copied()
                .unwrap_or(0);
            if self.ticket < slot.fence || self.ticket < retired {
                return;
            }
            let inserting = !slot.policy.contains_key(&decision.source);
            let entry = slot
                .policy
                .entry(decision.source.clone())
                .or_insert_with(|| PolicyRestriction {
                    opaque_id,
                    ..PolicyRestriction::default()
                });
            if self.ticket < entry.observe_fence {
                return;
            }
            let first = entry.next_probe.is_none();
            if probing {
                entry.failures = entry.failures.saturating_add(1);
            } else if first {
                entry.failures = 1;
            }
            if probing || first {
                let delay =
                    policy_backoff_secs(decision.backoff, entry.failures, key.generation[0]);
                let next = mono + Duration::from_secs(delay);
                entry.next_probe = Some(entry.next_probe.map_or(next, |old| old.max(next)));
                entry.backoff = Some(decision.backoff);
            }
            entry.revision = entry.revision.saturating_add(1);
            inserting
        };
        if inserting {
            inner.restriction_seq = next_seq;
        }
        self.observed.push(key);
    }

    /// Independent upstream Retry-After on the policy resource. Local waits,
    /// source clear, and rule deletion must not shorten this deadline.
    pub(crate) fn observe_policy_retry(
        &mut self,
        scope: RestrictionScope,
        hint: RetryHint,
        mono: Instant,
    ) {
        let Some(key) = self.resources.policy_key(scope) else {
            return;
        };
        self.observe_key(
            key,
            FailureDecision {
                persist_reset: None,
                wait_for_recovery: false,
                retry_not_before: Some(hint),
                exhaust_free: false,
            },
            mono,
        );
    }

    fn observe_key(&mut self, key: ResourceKey, decision: FailureDecision, mono: Instant) {
        if !decision.wait_for_recovery && decision.retry_not_before.is_none() {
            return;
        }
        let mut inner = self.runtime.inner.lock();
        let Some(slot) = inner.slots.get_mut(&key) else {
            return;
        };
        if self.ticket < slot.fence {
            return;
        }
        let probing = self
            .claims
            .iter()
            .any(|claim| claim.key == key && claim.probe);
        if decision.wait_for_recovery {
            if !slot.awaiting_recovery || probing {
                slot.failures = slot.failures.saturating_add(1);
            }
            slot.awaiting_recovery = true;
            // Bounded local probe policy, explicitly not an upstream reset.
            let base = INITIAL_PROBE_SECS
                .saturating_mul(1u64 << slot.failures.saturating_sub(1).min(4))
                .min(MAX_PROBE_SECS);
            let jitter = u64::from(key.generation[0]) % (base / 10 + 1);
            let next = mono + Duration::from_secs((base + jitter).min(MAX_PROBE_SECS));
            slot.next_probe = Some(slot.next_probe.map_or(next, |old| old.max(next)));
        }
        if let Some(hint) = decision.retry_not_before {
            slot.upstream_not_before =
                Some(slot.upstream_not_before.map_or(hint, |old| old.max(hint)));
        }
        slot.revision = slot.revision.wrapping_add(1);
        self.observed.push(key);
    }

    /// Only a complete, protocol-valid response may confirm a leased probe.
    /// A success admitted before a later failure cannot clear that failure.
    /// Policy probes drop only restriction episodes actually leased at acquire
    /// whose opaque id and revision are unchanged. Configured rules are not
    /// an episode set: a source recorded after this lease started stays.
    pub(crate) fn confirm_success(&mut self) {
        let mut inner = self.runtime.inner.lock();
        for claim in self.claims.iter().filter(|claim| claim.probe) {
            let Some(slot) = inner.slots.get_mut(&claim.key) else {
                continue;
            };
            if slot.probe_owner != Some(self.ticket) {
                continue;
            }
            if claim.key.kind.is_policy() {
                for leased in &claim.leased_policy {
                    let matches = slot.policy.get(&leased.source).is_some_and(|row| {
                        row.opaque_id == leased.opaque_id && row.revision == leased.revision
                    });
                    if matches {
                        slot.policy.remove(&leased.source);
                    }
                }
                if !slot.restricted() {
                    slot.probe_owner = None;
                }
                self.observed.push(claim.key.clone());
            } else if slot.revision == claim.revision && self.ticket >= slot.fence {
                slot.clear(self.ticket.saturating_add(1));
                self.observed.push(claim.key.clone());
            }
        }
    }
}
impl Drop for RecoveryPermit {
    fn drop(&mut self) {
        let mut inner = self.runtime.inner.lock();
        for claim in &self.claims {
            if let Some(slot) = inner.slots.get_mut(&claim.key) {
                if slot.probe_owner == Some(self.ticket) {
                    slot.probe_owner = None;
                    // Cancellation, malformed 2xx, or an unrelated error is not
                    // proof of recovery. Leave a small local recheck interval.
                    if !self.observed.contains(&claim.key) && slot.restricted() {
                        let next = self.cancelled_probe_at + self.lease_started.elapsed();
                        if claim.key.kind.is_policy() {
                            for leased in &claim.leased_policy {
                                if let Some(restriction) = slot.policy.get_mut(&leased.source)
                                    && restriction.opaque_id == leased.opaque_id
                                    && restriction.revision == leased.revision
                                    && restriction.next_probe.is_some()
                                {
                                    restriction.next_probe = Some(
                                        restriction.next_probe.map_or(next, |at| at.max(next)),
                                    );
                                }
                            }
                        } else {
                            slot.next_probe = Some(slot.next_probe.map_or(next, |at| at.max(next)));
                        }
                    }
                }
                slot.active = slot.active.saturating_sub(1);
            }
        }
        inner
            .slots
            .retain(|_, slot| slot.active != 0 || slot.restricted());
    }
}

fn kind_for(facts: &FailureFacts) -> ResourceKind {
    match facts.scope {
        Scope::Unspecified => ResourceKind::EndpointModel,
        Scope::SharedFreeEgress => ResourceKind::FreeEgress,
        Scope::QuotaPool => match facts.window {
            Some(UsageWindowKind::FiveHours) => ResourceKind::FiveHours,
            Some(UsageWindowKind::Week) => ResourceKind::Week,
            Some(UsageWindowKind::Month) => ResourceKind::Month,
            Some(UsageWindowKind::Free) => ResourceKind::FreeEgress,
            None => ResourceKind::Credits,
        },
    }
}

#[cfg(test)]
mod tests;
