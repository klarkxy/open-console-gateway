//! Process-local admission for observed restrictions, separate from quota rows.
//! Only real client requests probe. Pending resources admit one probe at a time;
//! healthy resources remain concurrent. Dropping a request always releases it.
use super::failure::{FailureDecision, FailureFacts, RetryHint, Scope};
use crate::models::UsageWindowKind;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod context;
pub(crate) use context::ResourceSet;

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
}
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct ResourceKey {
    kind: ResourceKind,
    generation: [u8; 32],
}

#[derive(Default)]
pub(crate) struct RecoveryRuntime {
    inner: Mutex<Inner>,
}
#[derive(Default)]
struct Inner {
    sequence: u64,
    slots: HashMap<ResourceKey, Slot>,
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
}
impl Slot {
    fn restricted(&self) -> bool {
        self.awaiting_recovery || self.upstream_not_before.is_some() || self.next_probe.is_some()
    }
    fn clear(&mut self, fence: u64) {
        self.awaiting_recovery = false;
        self.upstream_not_before = None;
        self.next_probe = None;
        self.probe_owner = None;
        self.failures = 0;
        self.fence = self.fence.max(fence);
        self.revision = self.revision.wrapping_add(1);
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct WaitState {
    pub reason: &'static str,
    pub upstream_not_before: Option<RetryHint>,
    pub next_probe_in_seconds: Option<u64>,
    pub probe_in_flight: bool,
}
impl WaitState {
    fn capacity() -> Self {
        Self {
            reason: "recovery_capacity",
            upstream_not_before: None,
            next_probe_in_seconds: None,
            probe_in_flight: false,
        }
    }
}
struct Claim {
    key: ResourceKey,
    revision: u64,
    probe: bool,
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
            // A credential not-before has no probe to clear it. Once elapsed,
            // release it so expired hints cannot accumulate toward capacity.
            if key.kind == ResourceKind::CredentialRetry
                && matches!(slot.upstream_not_before, Some(RetryHint::Until(at)) if at <= wall)
            {
                slot.upstream_not_before = None;
                if slot.active == 0 {
                    return false;
                }
            }
            matches!(
                key.kind,
                ResourceKind::EndpointModel | ResourceKind::FreeEgress
            ) || slot.active != 0
                || slot.owners != resources.owners(key)
                || slot.owner_generation == resources.owner_generation(key)
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
            if let Some(slot) = inner.slots.get(key) {
                let upstream_wait = match slot.upstream_not_before {
                    Some(RetryHint::Unbounded) => true,
                    Some(RetryHint::Until(at)) => at > wall,
                    None => false,
                };
                let local_wait = slot.next_probe.is_some_and(|at| at > mono);
                if upstream_wait || local_wait || slot.probe_owner.is_some() {
                    return Err(WaitState {
                        reason: "resource_waiting_for_recovery",
                        upstream_not_before: slot.upstream_not_before,
                        next_probe_in_seconds: slot.next_probe.map(|at| {
                            at.saturating_duration_since(mono)
                                .as_secs()
                                .saturating_add(1)
                        }),
                        probe_in_flight: slot.probe_owner.is_some(),
                    });
                }
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
                let probe = key.kind != ResourceKind::CredentialRetry
                    && resources.enforces(&key)
                    && slot.restricted();
                if probe {
                    slot.probe_owner = Some(ticket);
                }
                Claim {
                    key,
                    revision: slot.revision,
                    probe,
                }
            })
            .collect();
        Ok(RecoveryPermit {
            runtime: self.clone(),
            resources,
            claims,
            ticket,
            cancelled_probe_at: mono + Duration::from_secs(INITIAL_PROBE_SECS),
            observed: Vec::new(),
            lease_started: Instant::now(),
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

    pub(crate) fn same_generation(&self, resources: &ResourceSet) -> bool {
        self.resources.same_generation(resources)
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
    pub(crate) fn confirm_success(&mut self) {
        let mut inner = self.runtime.inner.lock();
        for claim in self.claims.iter().filter(|claim| claim.probe) {
            if let Some(slot) = inner.slots.get_mut(&claim.key)
                && slot.revision == claim.revision
                && slot.probe_owner == Some(self.ticket)
                && self.ticket >= slot.fence
            {
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
                        slot.next_probe = Some(slot.next_probe.map_or(next, |at| at.max(next)));
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
