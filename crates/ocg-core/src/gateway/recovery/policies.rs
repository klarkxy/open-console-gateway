//! Configurable rules use the same admission slots, leases and success proof as
//! built-in recovery, but never borrow an endpoint-wide or shared quota identity.
use super::*;
use crate::db::temporary_policy::{SavedRules, VersionedRule};
use crate::routing_snapshot::RoutingSnapshot;
use crate::temporary_policy::{TemporaryRuleScope, TemporaryWait, TemporaryWaitStatus};
use ocg_gateway::classify::ProviderErrorClass;

#[derive(Clone)]
pub(super) struct PolicyResource {
    pub key: ResourceKey,
    pub retry_key: ResourceKey,
    pub credential_key: ResourceKey,
    pub rule: VersionedRule,
    pub credential_id: String,
    pub account_id: String,
    pub destination_id: String,
    pub endpoint: String,
    pub model: String,
}

#[derive(Serialize)]
pub(crate) struct PolicyReceipt {
    rule_id: String,
    rule_version: u64,
    scope: TemporaryRuleScope,
    local_reprobe: bool,
}

impl RecoveryPermit {
    /// The caller rechecks the live send identity under the database lock.
    /// Each rule is then version-checked independently: editing rule A must not
    /// discard evidence for unchanged rule B. Header waits have their own slot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_policies(
        &mut self,
        current: &ResourceSet,
        status: u16,
        class: ProviderErrorClass,
        body: Option<&str>,
        retry_hint: Option<RetryHint>,
        mono: Instant,
    ) -> Vec<PolicyReceipt> {
        let candidates = self.resources.policies.clone();
        let mut receipts = Vec::new();
        for policy in candidates {
            if current.policy_for(&policy.key).is_none()
                || !policy.rule.rule.matches(status, class, body)
                || !self
                    .runtime
                    .inner
                    .lock()
                    .slots
                    .get(&policy.key)
                    .is_some_and(|slot| self.ticket >= slot.fence)
            {
                continue;
            }
            self.observe_key_with_backoff(
                policy.key,
                FailureDecision {
                    persist_reset: None,
                    wait_for_recovery: true,
                    retry_not_before: None,
                    exhaust_free: false,
                },
                mono,
                policy.rule.rule.initial_seconds,
                policy.rule.rule.max_seconds,
            );
            receipts.push(PolicyReceipt {
                rule_id: policy.rule.rule.id,
                rule_version: policy.rule.revision,
                scope: policy.rule.rule.scope,
                local_reprobe: true,
            });
        }
        if !receipts.is_empty() {
            // Clearing/editing a rule cannot shorten an independently advertised
            // Retry-After. Its narrow credential+route+model header slot has no probe.
            self.observe_key(
                self.resources.key(ResourceKind::CredentialModelRetry),
                FailureDecision {
                    persist_reset: None,
                    wait_for_recovery: false,
                    retry_not_before: retry_hint,
                    exhaust_free: false,
                },
                mono,
            );
        }
        receipts
    }
}

impl RecoveryRuntime {
    /// Retire only changed/removed rule versions and fence their in-flight replies.
    /// No quota, header-only, other-rule, enablement or authentication state changes.
    pub(crate) fn reconcile_policies(&self, rules: &SavedRules) {
        let mut inner = self.inner.lock();
        let fence = inner.sequence.saturating_add(1);
        for slot in inner.slots.values_mut() {
            if slot.policy.as_ref().is_some_and(|policy| {
                !rules.contains_effective(&policy.destination_id, &policy.rule)
            }) {
                slot.clear(fence);
            }
        }
        inner
            .slots
            .retain(|_, slot| slot.active != 0 || slot.restricted());
    }

    /// A diagnostic id includes the slot revision. Stale UI reads cannot clear a
    /// newer rejection. The HTTP caller additionally checks process-generation CAS.
    pub(crate) fn reset_policy_wait(&self, id: &str) -> bool {
        let mut inner = self.inner.lock();
        let fence = inner.sequence.saturating_add(1);
        let mut found = false;
        for slot in inner.slots.values_mut() {
            if slot.policy.is_some()
                && slot.restricted()
                && format!("{}:{}", slot.policy_id, slot.revision) == id
            {
                slot.clear(fence);
                found = true;
                break;
            }
        }
        inner
            .slots
            .retain(|_, slot| slot.active != 0 || slot.restricted());
        found
    }

    /// Read-only. No lease claim, TTL touch, inference or inferred health. Filter
    /// rotated credentials, changed bindings/routes and inactive rule versions.
    pub(crate) fn policy_waits(
        &self,
        snapshot: &RoutingSnapshot,
        rules: &SavedRules,
        wall: DateTime<Utc>,
        mono: Instant,
    ) -> Vec<TemporaryWait> {
        let inner = self.inner.lock();
        let mut waits = Vec::new();
        for slot in inner.slots.values() {
            let Some(policy) = &slot.policy else {
                continue;
            };
            if !slot.restricted() || !rules.contains_effective(&policy.destination_id, &policy.rule)
            {
                continue;
            }
            let Some(account) = snapshot
                .credentials
                .iter()
                .find(|account| account.id == policy.account_id)
            else {
                continue;
            };
            let Ok(resources) = ResourceSet::from_snapshot(
                snapshot,
                account,
                &policy.endpoint,
                &policy.model,
                false,
            ) else {
                continue;
            };
            if resources.credential_generation() != slot.owner_generation {
                continue;
            }
            let retry_hint = [&policy.retry_key, &policy.credential_key]
                .into_iter()
                .filter_map(|key| {
                    inner
                        .slots
                        .get(key)
                        .and_then(|slot| slot.upstream_not_before)
                })
                .max();
            let upstream_wait = matches!(retry_hint, Some(RetryHint::Unbounded))
                || matches!(retry_hint, Some(RetryHint::Until(at)) if at > wall);
            let local_wait = slot.next_probe.is_some_and(|at| at > mono);
            let status = if slot.probe_owner.is_some() {
                TemporaryWaitStatus::Probing
            } else if local_wait || upstream_wait {
                TemporaryWaitStatus::Waiting
            } else {
                TemporaryWaitStatus::Ready
            };
            waits.push(TemporaryWait {
                id: format!("{}:{}", slot.policy_id, slot.revision),
                rule_id: policy.rule.rule.id.clone(),
                rule_version: policy.rule.revision,
                credential_id: policy.credential_id.clone(),
                credential_name: account.name.clone(),
                destination_id: policy.destination_id.clone(),
                model: (policy.rule.rule.scope == TemporaryRuleScope::CredentialModel)
                    .then(|| policy.model.clone()),
                status,
                next_probe_in_seconds: slot.next_probe.map(|at| {
                    let wait = at.saturating_duration_since(mono);
                    wait.as_secs()
                        .saturating_add(u64::from(wait.subsec_nanos() != 0))
                }),
                retry_not_before: match retry_hint {
                    Some(RetryHint::Until(at)) => Some(at.to_rfc3339()),
                    _ => None,
                },
                retry_unbounded: retry_hint == Some(RetryHint::Unbounded),
                failures: slot.failures,
            });
        }
        waits.sort_by(|a, b| {
            (&a.destination_id, &a.credential_id, &a.rule_id, &a.model).cmp(&(
                &b.destination_id,
                &b.credential_id,
                &b.rule_id,
                &b.model,
            ))
        });
        waits
    }
}

#[cfg(test)]
mod tests;
