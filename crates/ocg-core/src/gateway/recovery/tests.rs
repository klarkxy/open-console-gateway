use super::*;
use crate::gateway::failure::decode::openrouter_free_rejection;
use crate::gateway::failure::{Cause, FailureFacts};

fn clock() -> (DateTime<Utc>, Instant) {
    (Utc::now(), Instant::now())
}
fn facts(scope: Scope, window: Option<UsageWindowKind>, retry: Option<RetryHint>) -> FailureFacts {
    FailureFacts {
        cause: if scope == Scope::Unspecified {
            Cause::Unknown
        } else if window.is_some() {
            Cause::QuotaExhausted
        } else {
            Cause::CreditsExhausted
        },
        scope,
        window,
        upstream_reset_at: None,
        retry_not_before: retry,
        rule_id: "test",
        rule_version: 1,
    }
}
fn credit() -> FailureFacts {
    facts(Scope::QuotaPool, None, None)
}
fn resource(generation: u8) -> ResourceSet {
    ResourceSet::fixture(generation, 9, 1, &["a"], false)
}

#[test]
fn account_reset_fences_delayed_policy_observations() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let resources = resource(1);
    let mut delayed = runtime.acquire(resources.clone(), wall, mono).unwrap();
    let decision = goat_policy();
    assert!(delayed.permits_policy(&decision));
    runtime.reset_account("a");
    assert!(!delayed.permits_policy(&decision));
    delayed.observe_policy(&decision, mono);
    drop(delayed);
    assert!(runtime.inspect_admission(&resources, wall, mono).is_ok());
    let mut fresh = runtime.acquire(resources.clone(), wall, mono).unwrap();
    assert!(fresh.permits_policy(&decision));
    fresh.observe_policy(&decision, mono);
    drop(fresh);
    assert!(runtime.inspect_admission(&resources, wall, mono).is_err());
}

#[test]
fn healthy_concurrency_and_first_credit_wait_do_not_change_other_accounts() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let mut a = runtime.acquire(resource(1), wall, mono).unwrap();
    let mut concurrent = runtime.acquire(resource(1), wall, mono).unwrap();
    let f = credit();
    a.observe_failure(&f, f.decide(), mono);
    concurrent.confirm_success(); // This was admitted before the failure.
    assert!(runtime.acquire(resource(1), wall, mono).is_err());
    assert!(
        runtime
            .acquire(ResourceSet::fixture(2, 9, 1, &["b"], false), wall, mono)
            .is_ok()
    );
    drop((a, concurrent));
    assert!(runtime.acquire(resource(1), wall, mono).is_err());
}

#[test]
fn due_probe_is_singleflight_and_only_its_success_recovers() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let f = credit();
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(resource(1), wall, later).unwrap();
    assert!(runtime.acquire(resource(1), wall, later).is_err());
    probe.confirm_success();
    drop(probe);
    assert!(runtime.acquire(resource(1), wall, later).is_ok());
    assert!(runtime.inner.lock().slots.is_empty());
}

#[test]
fn cancellation_and_unrelated_failure_do_not_certify_recovery() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let f = credit();
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    let later = mono + Duration::from_secs(40);
    let mut p = runtime.acquire(resource(1), wall, later).unwrap();
    let transient = facts(Scope::Unspecified, None, None);
    p.observe_failure(&transient, transient.decide(), later);
    drop(p);
    assert!(runtime.acquire(resource(1), wall, later).is_err());
    let p = runtime
        .acquire(resource(1), wall, later + Duration::from_secs(40))
        .unwrap();
    drop(p);
    assert!(
        runtime
            .acquire(resource(1), wall, later + Duration::from_secs(40))
            .is_err()
    );
}

#[test]
fn repeated_failure_backs_off_but_never_claims_a_quota_reset() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mut mono) = clock();
    let f = credit();
    for _ in 0..8 {
        let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
        p.observe_failure(&f, f.decide(), mono);
        drop(p);
        let wait = runtime.acquire(resource(1), wall, mono).err().unwrap();
        assert_eq!(wait.upstream_not_before, None);
        assert!(wait.next_probe_in_seconds.unwrap() <= MAX_PROBE_SECS + 1);
        mono += Duration::from_secs(310);
    }
}

#[test]
fn pool_model_and_endpoint_scopes_are_distinct() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = ResourceSet::fixture(1, 1, 1, &["a", "b"], false);
    let b = ResourceSet::fixture(1, 2, 1, &["a", "b"], false);
    let different_model = ResourceSet::fixture(1, 1, 2, &["a", "b"], false);
    let f = credit();
    let mut p = runtime.acquire(a.clone(), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(runtime.acquire(b.clone(), wall, mono).is_err());
    assert!(runtime.acquire(different_model.clone(), wall, mono).is_ok());
    runtime.reset_account("b");
    let f = facts(Scope::QuotaPool, Some(UsageWindowKind::Week), None);
    let mut p = runtime.acquire(a.clone(), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(runtime.acquire(different_model, wall, mono).is_err());
    runtime.reset_account("a");
    let f = facts(
        Scope::Unspecified,
        None,
        Some(RetryHint::Until(wall + chrono::Duration::seconds(90))),
    );
    let mut p = runtime.acquire(a, wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(
        runtime
            .acquire(ResourceSet::fixture(3, 1, 1, &["c"], false), wall, mono)
            .is_err()
    );
    assert!(runtime.acquire(b, wall, mono).is_ok());
}

#[test]
fn openrouter_free_wait_keeps_paid_models_other_keys_and_zen_available() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let free = ResourceSet::fixture(1, 9, 1, &["openrouter-a"], false);
    let paid = ResourceSet::fixture(1, 9, 2, &["openrouter-a"], false);
    let other_key = ResourceSet::fixture(2, 9, 1, &["openrouter-b"], false);
    let zen = ResourceSet::fixture(3, 9, 1, &["zen"], true);
    let facts = openrouter_free_rejection(None, wall);
    let mut permit = runtime.acquire(free.clone(), wall, mono).unwrap();
    permit.observe_failure(&facts, facts.decide(), mono);
    drop(permit);
    assert!(runtime.acquire(free, wall, mono).is_err());
    assert!(runtime.acquire(paid, wall, mono).is_ok());
    assert!(runtime.acquire(other_key, wall, mono).is_ok());
    assert!(runtime.acquire(zen, wall, mono).is_ok());
}

#[test]
fn long_upstream_wait_is_not_shortened_by_local_policy_or_reset_time() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let mut f = facts(
        Scope::QuotaPool,
        Some(UsageWindowKind::Week),
        Some(RetryHint::Until(wall + chrono::Duration::days(40))),
    );
    f.upstream_reset_at = Some(wall + chrono::Duration::days(1));
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(
        runtime
            .acquire(
                resource(1),
                wall + chrono::Duration::days(2),
                mono + Duration::from_secs(9999)
            )
            .is_err()
    );
    assert!(
        runtime
            .acquire(
                resource(1),
                wall + chrono::Duration::days(41),
                mono + Duration::from_secs(9999)
            )
            .is_ok()
    );
}

#[test]
fn operator_reset_fences_late_reply_and_free_scope_cannot_be_reset_from_account() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let f = credit();
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    runtime.reset_account("a");
    assert!(!p.permits_observation(&f));
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(runtime.acquire(resource(1), wall, mono).is_ok());
    let free = ResourceSet::fixture(1, 1, 1, &["free"], true);
    let f = facts(Scope::SharedFreeEgress, Some(UsageWindowKind::Free), None);
    let mut p = runtime.acquire(free.clone(), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    runtime.reset_account("free");
    assert!(runtime.acquire(free, wall, mono).is_err());
    assert!(runtime.acquire(resource(1), wall, mono).is_ok());
    assert!(
        runtime
            .free_egress_retry_until(wall, mono)
            .is_some_and(|until| until > wall),
        "shared Free waits must expose a soonest deadline for all-waiting 429"
    );
}

#[test]
fn rotated_generation_does_not_inherit_state_and_old_state_is_reclaimed() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let f = credit();
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    assert!(!runtime.inner.lock().slots.is_empty());
    let p = runtime.acquire(resource(2), wall, mono).unwrap();
    assert!(!p.same_generation(&resource(1)));
    drop(p);
    assert!(runtime.inner.lock().slots.is_empty());
}

#[test]
fn concurrency_has_one_probe_and_no_lock_held_across_send() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let f = credit();
    let mut p = runtime.acquire(resource(1), wall, mono).unwrap();
    p.observe_failure(&f, f.decide(), mono);
    drop(p);
    let admitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let barrier = Arc::new(std::sync::Barrier::new(9));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let runtime = runtime.clone();
            let admitted = admitted.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                let p = runtime.acquire(resource(1), wall, mono + Duration::from_secs(40));
                if p.is_ok() {
                    admitted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
                barrier.wait();
                drop(p);
            });
        }
        barrier.wait();
        assert_eq!(admitted.load(std::sync::atomic::Ordering::SeqCst), 1);
    });
}

#[test]
fn persistent_quota_retry_hint_is_per_key_and_does_not_claim_a_second_probe() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = ResourceSet::fixture(1, 9, 1, &["a", "b"], false).with_credential(2, "a");
    let b = a.clone().with_credential(3, "b");
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_credential_retry(
        Some(RetryHint::Until(wall + chrono::Duration::days(60))),
        mono,
    );
    drop(permit);
    assert!(runtime.acquire(a.clone(), wall, mono).is_err());
    assert!(runtime.acquire(b, wall, mono).is_ok());
    runtime.reset_account("b");
    assert!(runtime.acquire(a.clone(), wall, mono).is_err());
    let later = wall + chrono::Duration::days(61);
    let first = runtime.acquire(a.clone(), later, mono).unwrap();
    let second = runtime.acquire(a.clone(), later, mono).unwrap();
    assert!(first.claims.iter().all(|claim| !claim.probe));
    drop((first, second));
    assert!(runtime.acquire(a, later, mono).is_ok());
    assert!(
        runtime.inner.lock().slots.is_empty(),
        "elapsed hints must be reclaimed"
    );
}

#[test]
fn quota_observation_fence_is_owned_by_the_selected_key() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = ResourceSet::fixture(1, 9, 1, &["a", "b"], false).with_credential(2, "a");
    let permit = runtime.acquire(a, wall, mono).unwrap();
    let observation = permit.quota_observation();
    runtime.reset_account("b");
    assert!(observation.is_current());
    runtime.reset_account("a");
    assert!(!observation.is_current());
}

fn goat_policy() -> crate::gateway::policy::PolicyDecision {
    crate::gateway::policy::evaluate(
        &crate::gateway::policy::EffectivePolicySnapshot::builtin(),
        "dest-1",
        &crate::gateway::policy::PolicyInput {
            adapter: ocg_domain::provider::ProviderAdapterKind::CommandCodeGoat,
            class: ocg_gateway::classify::ProviderErrorClass::InsufficientCredits,
            http_status: Some(400),
            error: None,
        },
    )
    .into_iter()
    .next()
    .expect("builtin goat")
}

fn extra_source(rule_id: &str, generation: u64) -> crate::gateway::policy::PolicySource {
    crate::gateway::policy::PolicySource {
        owner: crate::gateway::policy::GLOBAL_OWNER.into(),
        rule_id: rule_id.into(),
        rule_generation: generation,
    }
}

fn extra_rule(
    source: crate::gateway::policy::PolicySource,
) -> crate::gateway::policy::EffectivePolicyRule {
    crate::gateway::policy::EffectivePolicyRule {
        source,
        destination_id: None,
        enabled: true,
        scope: crate::gateway::policy::RestrictionScope::CredentialModel,
        action: crate::gateway::policy::PolicyAction::TemporaryUnavailable,
        matcher: crate::gateway::policy::PolicyMatcher::GoatInsufficientCredits,
        backoff: crate::gateway::policy::PolicyBackoff::default(),
    }
}

fn snapshot_with(
    extra: &[crate::gateway::policy::EffectivePolicyRule],
) -> crate::gateway::policy::EffectivePolicySnapshot {
    let mut snapshot = crate::gateway::policy::EffectivePolicySnapshot::builtin();
    snapshot.layers.extend(extra.iter().cloned());
    snapshot
}

fn bump_builtin_generation() -> crate::gateway::policy::EffectivePolicySnapshot {
    crate::gateway::policy::compile_snapshot(
        &[crate::gateway::policy::ConfiguredRule::BuiltinOverride {
            id: crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE.into(),
            destination_id: None,
            enabled: true,
            backoff: Some(crate::gateway::policy::PolicyBackoff {
                initial_secs: 31,
                max_secs: 300,
            }),
        }],
        &crate::gateway::policy::EffectivePolicySnapshot::builtin(),
        2,
    )
}

#[test]
fn goat_policy_waits_are_per_credential_model_and_skip_without_lease() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = ResourceSet::fixture(1, 1, 1, &["a", "pool"], false);
    let other_model = ResourceSet::fixture(1, 1, 2, &["a", "pool"], false);
    let sibling = a.clone().with_credential(9, "pool");
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    drop(permit);
    runtime
        .inspect_admission(&a, wall, mono)
        .expect_err("waiting");
    assert!(runtime.acquire(other_model.clone(), wall, mono).is_ok());
    assert!(runtime.acquire(sibling, wall, mono).is_ok());
    let inspect = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(inspect.is_local_policy());
    assert_eq!(inspect.skip_stage(), "local_policy_skip");
    assert!(!inspect.sources.is_empty());
    assert!(inspect.sources.iter().all(|s| s.owner == "builtin"));
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    assert!(runtime.inspect_admission(&a, wall, later).is_err());
    probe.confirm_success();
    drop(probe);
    assert!(runtime.inspect_admission(&a, wall, later).is_ok());
}

#[test]
fn concurrent_policy_failures_do_not_exponentiate_only_probe_failures_do() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let first = runtime.acquire(a.clone(), wall, mono).unwrap();
    let second = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut first = first;
    let mut second = second;
    first.observe_policy(&goat_policy(), mono);
    second.observe_policy(&goat_policy(), mono);
    drop((first, second));
    let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(wait.next_probe_in_seconds.unwrap() <= 40);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    probe.observe_policy(&goat_policy(), later);
    drop(probe);
    let wait = runtime.inspect_admission(&a, wall, later).unwrap_err();
    assert!(wait.next_probe_in_seconds.unwrap() > 40);
}

#[test]
fn missing_model_does_not_widen_to_credential_scope() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let missing = resource(1).without_model();
    let mut permit = runtime.acquire(missing.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    drop(permit);
    assert!(runtime.inspect_admission(&missing, wall, mono).is_ok());
}

#[test]
fn cancel_and_unrelated_success_do_not_clear_policy() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = ResourceSet::fixture(1, 1, 1, &["a"], false);
    let other = ResourceSet::fixture(1, 1, 2, &["a"], false);
    let mut p = runtime.acquire(a.clone(), wall, mono).unwrap();
    p.observe_policy(&goat_policy(), mono);
    drop(p);
    let later = mono + Duration::from_secs(40);
    let p = runtime.acquire(a.clone(), wall, later).unwrap();
    drop(p);
    assert!(runtime.inspect_admission(&a, wall, later).is_err());
    let mut other_ok = runtime.acquire(other, wall, later).unwrap();
    other_ok.confirm_success();
    drop(other_ok);
    assert!(runtime.inspect_admission(&a, wall, later).is_err());
}

#[test]
fn rule_generation_change_fences_old_observe_and_success() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let mut stale = runtime.acquire(a.clone(), wall, mono).unwrap();
    let next = bump_builtin_generation();
    let live_source = next
        .effective_for(&a.destination_id)
        .into_iter()
        .find(|rule| rule.source.rule_id == crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE)
        .unwrap()
        .source;
    runtime.install_snapshot(next);
    stale.observe_policy(&goat_policy(), mono);
    drop(stale);
    assert!(runtime.inspect_admission(&a, wall, mono).is_ok());
    assert_eq!(runtime.policy_snapshot().epoch, 2);
    let mut live = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut current = goat_policy();
    current.source = live_source.clone();
    live.observe_policy(&current, mono);
    drop(live);
    assert!(runtime.inspect_admission(&a, wall, mono).is_err());
    let later = mono + Duration::from_secs(40);
    let mut stale_probe = runtime.acquire(a.clone(), wall, later).unwrap();
    let next = crate::gateway::policy::compile_snapshot(
        &[crate::gateway::policy::ConfiguredRule::BuiltinOverride {
            id: crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE.into(),
            destination_id: None,
            enabled: true,
            backoff: Some(crate::gateway::policy::PolicyBackoff {
                initial_secs: 32,
                max_secs: 300,
            }),
        }],
        &runtime.policy_snapshot(),
        3,
    );
    let newest = next
        .effective_for(&a.destination_id)
        .into_iter()
        .find(|rule| rule.source.rule_id == crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE)
        .unwrap()
        .source;
    runtime.install_snapshot(next);
    stale_probe.confirm_success();
    drop(stale_probe);
    let mut live = runtime.acquire(a.clone(), wall, later).unwrap();
    let mut current = goat_policy();
    current.source = newest;
    live.observe_policy(&current, later);
    drop(live);
    assert!(runtime.inspect_admission(&a, wall, later).is_err());
}

#[test]
fn removing_rule_a_does_not_clear_rule_b_or_retry_after() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let extra = extra_source("keep-b", 1);
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    let keep = crate::gateway::policy::PolicyDecision {
        action: PolicyAction::TemporaryUnavailable,
        scope: RestrictionScope::CredentialModel,
        source: extra.clone(),
        backoff: crate::gateway::policy::PolicyBackoff::default(),
    };
    permit.observe_policy(&keep, mono);
    permit.observe_credential_retry(
        Some(RetryHint::Until(wall + chrono::Duration::seconds(90))),
        mono,
    );
    drop(permit);
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(!wait.is_local_policy(), "{wait:?}");
    assert!(wait.upstream_not_before.is_some());
    runtime.clear_source(&extra);
    let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(wait.upstream_not_before.is_some());
    assert!(wait.sources.is_empty());
}

#[test]
fn disabling_a_rule_fences_that_source_and_leaves_others() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let extra = extra_source("keep-b", 1);
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    permit.observe_policy(
        &crate::gateway::policy::PolicyDecision {
            action: PolicyAction::TemporaryUnavailable,
            scope: RestrictionScope::CredentialModel,
            source: extra.clone(),
            backoff: crate::gateway::policy::PolicyBackoff::default(),
        },
        mono,
    );
    drop(permit);
    let mut disabled = snapshot_with(&[extra_rule(extra)]);
    for layer in &mut disabled.layers {
        if layer.source.rule_id == crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE {
            layer.enabled = false;
        }
    }
    runtime.install_snapshot(disabled);
    let inspect = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(inspect.is_local_policy());
    assert_eq!(inspect.sources.len(), 1);
    assert_eq!(inspect.sources[0].rule_id, "keep-b");
}

#[test]
fn old_credential_generation_is_reclaimed_and_capacity_is_explicit() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let old = resource(1);
    let mut permit = runtime.acquire(old, wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    drop(permit);
    let replaced = resource(2);
    let permit = runtime.acquire(replaced, wall, mono).unwrap();
    drop(permit);
    assert!(runtime.inspect_admission(&resource(1), wall, mono).is_ok());
    let mut stored = 0;
    for index in 0..5000u16 {
        let set = ResourceSet::unique(index, &["cap"]);
        match runtime.inspect_admission(&set, wall, mono) {
            Err(wait) => {
                assert!(wait.is_capacity(), "{wait:?}");
                assert!(stored > 0);
                assert!(runtime.tracked_slot_count() <= 4096);
                return;
            }
            Ok(()) => {
                let mut permit = runtime.acquire(set, wall, mono).unwrap();
                permit.observe_policy(&goat_policy(), mono);
                drop(permit);
                stored += 1;
            }
        }
    }
    panic!("expected an explicit capacity wait after {stored} restrictions");
}

#[test]
fn inspect_does_not_take_a_probe_lease() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    drop(permit);
    let later = mono + Duration::from_secs(40);
    runtime.inspect_admission(&a, wall, later).unwrap();
    runtime.inspect_admission(&a, wall, later).unwrap();
    let first = runtime.acquire(a.clone(), wall, later).unwrap();
    assert!(runtime.acquire(a, wall, later).is_err());
    drop(first);
}

#[test]
fn stale_policy_observation_does_not_bind_a_rotated_credential() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let old = resource(1);
    let new = resource(2);
    let mut stale = runtime.acquire(old.clone(), wall, mono).unwrap();
    assert!(!stale.same_generation(&new));
    let live = runtime.acquire(new.clone(), wall, mono).unwrap();
    stale.observe_policy(&goat_policy(), mono);
    drop((stale, live));
    assert!(runtime.inspect_admission(&new, wall, mono).is_ok());
}

#[test]
fn pool_rotation_does_not_drop_selected_key_policy_observe() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let selected = resource(1);
    let rotated_pool = selected.clone().with_quota_pool(9);
    assert!(!selected.same_generation(&rotated_pool));
    assert!(selected.same_policy_identity(&rotated_pool));
    let mut permit = runtime.acquire(selected, wall, mono).unwrap();
    assert!(permit.same_policy_identity(&rotated_pool));
    permit.observe_policy(&goat_policy(), mono);
    drop(permit);
    assert!(
        runtime
            .inspect_admission(&rotated_pool, wall, mono)
            .is_err()
    );
}

#[test]
fn connection_override_fences_a_and_keeps_b() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let dest_a = ResourceSet::fixture(1, 1, 1, &["a"], false);
    let dest_b = ResourceSet::fixture(2, 2, 1, &["b"], false);
    let mut in_flight_a = runtime.acquire(dest_a.clone(), wall, mono).unwrap();
    let mut recorded_b = runtime.acquire(dest_b.clone(), wall, mono).unwrap();
    recorded_b.observe_policy(&goat_policy(), mono);
    drop(recorded_b);
    assert!(runtime.inspect_admission(&dest_b, wall, mono).is_err());
    let override_a = crate::gateway::policy::compile_snapshot(
        &[crate::gateway::policy::ConfiguredRule::BuiltinOverride {
            id: crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE.into(),
            destination_id: Some(dest_a.destination_id.clone()),
            enabled: false,
            backoff: None,
        }],
        &runtime.policy_snapshot(),
        2,
    );
    runtime.install_snapshot(override_a);
    in_flight_a.observe_policy(&goat_policy(), mono);
    drop(in_flight_a);
    assert!(runtime.inspect_admission(&dest_a, wall, mono).is_ok());
    assert!(runtime.inspect_admission(&dest_b, wall, mono).is_err());
}

#[test]
fn clearing_source_a_does_not_fence_in_flight_source_b() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let extra = extra_source("keep-b", 1);
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut second = runtime.acquire(a.clone(), wall, mono).unwrap();
    first.observe_policy(&goat_policy(), mono);
    drop(first);
    runtime.clear_source(&goat_policy().source);
    second.observe_policy(
        &crate::gateway::policy::PolicyDecision {
            action: PolicyAction::TemporaryUnavailable,
            scope: RestrictionScope::CredentialModel,
            source: extra.clone(),
            backoff: crate::gateway::policy::PolicyBackoff::default(),
        },
        mono,
    );
    drop(second);
    let inspect = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(inspect.is_local_policy());
    assert_eq!(inspect.sources.len(), 1);
    assert_eq!(inspect.sources[0].rule_id, "keep-b");
}

#[test]
fn probe_success_does_not_clear_a_source_recorded_after_acquire() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let extra = extra_source("late-b", 1);
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    first.observe_policy(&goat_policy(), mono);
    drop(first);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    probe.observe_policy(
        &crate::gateway::policy::PolicyDecision {
            action: PolicyAction::TemporaryUnavailable,
            scope: RestrictionScope::CredentialModel,
            source: extra.clone(),
            backoff: crate::gateway::policy::PolicyBackoff::default(),
        },
        later,
    );
    probe.confirm_success();
    drop(probe);
    let inspect = runtime.inspect_admission(&a, wall, later).unwrap_err();
    assert!(
        inspect
            .sources
            .iter()
            .any(|source| source.rule_id == "late-b")
    );
    assert!(
        inspect
            .sources
            .iter()
            .all(|source| source.rule_id != crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE)
    );
}

#[test]
fn configurable_policy_backoff_reaches_max_without_exponent_cap() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mut mono) = clock();
    let a = resource(1);
    let source = extra_source("tiny-backoff", 1);
    let backoff = crate::gateway::policy::PolicyBackoff {
        initial_secs: 1,
        max_secs: 64,
    };
    runtime.install_snapshot(snapshot_with(&[
        crate::gateway::policy::EffectivePolicyRule {
            source: source.clone(),
            destination_id: None,
            enabled: true,
            scope: RestrictionScope::CredentialModel,
            action: PolicyAction::TemporaryUnavailable,
            matcher: crate::gateway::policy::PolicyMatcher::GoatInsufficientCredits,
            backoff,
        },
    ]));
    let decision = crate::gateway::policy::PolicyDecision {
        action: PolicyAction::TemporaryUnavailable,
        scope: RestrictionScope::CredentialModel,
        source,
        backoff,
    };
    let mut seen_max = None;
    for _ in 0..12 {
        let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
        permit.observe_policy(&decision, mono);
        drop(permit);
        let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
        let secs = wait.next_probe_in_seconds.expect("policy wait");
        seen_max = Some(seen_max.unwrap_or(0).max(secs));
        if secs >= 64 {
            break;
        }
        mono += Duration::from_secs(secs);
    }
    assert!(
        seen_max.unwrap_or(0) >= 64,
        "initial=1 max=64 must reach 64, got {seen_max:?}"
    );
}

#[test]
fn inspect_capacity_projects_rotated_generation_reclaim() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let mut stored = 0;
    for index in 0..5000u16 {
        let set = ResourceSet::unique(index, &["cap"]);
        match runtime.inspect_admission(&set, wall, mono) {
            Err(wait) => {
                assert!(wait.is_capacity(), "{wait:?}");
                stored = index;
                break;
            }
            Ok(()) => {
                let mut permit = runtime.acquire(set, wall, mono).unwrap();
                permit.observe_policy(&goat_policy(), mono);
                drop(permit);
            }
        }
    }
    assert!(stored > 0);
    let extra = ResourceSet::unique(stored, &["cap"]);
    assert!(
        runtime.inspect_admission(&extra, wall, mono).is_err(),
        "full table still rejects a brand-new identity"
    );
    let rotated = ResourceSet::unique(0, &["cap"]).with_credential(255, "cap-0");
    runtime
        .inspect_admission(&rotated, wall, mono)
        .expect("rotated generation must reclaim the obsolete slot");
    runtime
        .acquire(rotated, wall, mono)
        .expect("acquire must run the same reclaim as inspect");
}

#[test]
fn inspect_capacity_projects_expired_credential_retry_reclaim() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let mut stored = 0u16;
    for index in 0..5000u16 {
        let set = ResourceSet::unique(index, &["exp"]);
        if runtime.inspect_admission(&set, wall, mono).is_err() {
            stored = index;
            break;
        }
        let mut permit = runtime.acquire(set, wall, mono).unwrap();
        permit.observe_policy(&goat_policy(), mono);
        drop(permit);
    }
    assert!(stored > 1);
    // Free one policy slot, then occupy it with an already-elapsed Key retry.
    let last = ResourceSet::unique(stored.saturating_sub(1), &["exp"]);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(last.clone(), wall, later).unwrap();
    probe.confirm_success();
    drop(probe);
    let mut retry = runtime.acquire(last.clone(), wall, later).unwrap();
    retry.observe_credential_retry(Some(RetryHint::Until(wall)), later);
    drop(retry);
    let extra = ResourceSet::unique(stored, &["exp"]);
    runtime
        .inspect_admission(&extra, wall, later)
        .expect("expired credential retry must not freeze capacity");
    runtime
        .acquire(extra, wall, later)
        .expect("acquire must reclaim the expired retry slot");
}

#[test]
fn probe_success_does_not_clear_already_configured_source_recorded_after_lease() {
    use crate::gateway::policy::{PolicyAction, RestrictionScope};
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let extra = extra_source("configured-b", 1);
    runtime.install_snapshot(snapshot_with(&[extra_rule(extra.clone())]));
    let mut pending = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    first.observe_policy(&goat_policy(), mono);
    drop(first);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    pending.observe_policy(
        &crate::gateway::policy::PolicyDecision {
            action: PolicyAction::TemporaryUnavailable,
            scope: RestrictionScope::CredentialModel,
            source: extra.clone(),
            backoff: crate::gateway::policy::PolicyBackoff::default(),
        },
        later,
    );
    drop(pending);
    probe.confirm_success();
    drop(probe);
    let inspect = runtime.inspect_admission(&a, wall, later).unwrap_err();
    assert!(
        inspect
            .sources
            .iter()
            .any(|source| source.rule_id == "configured-b"),
        "{inspect:?}"
    );
}

#[test]
fn probe_success_does_not_clear_aba_recreated_episode() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    first.observe_policy(&goat_policy(), mono);
    drop(first);
    let id = runtime
        .list_restrictions(mono)
        .into_iter()
        .next()
        .expect("leased restriction")
        .id;
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    runtime.clear_restriction(&id);
    let mut recreate = runtime.acquire(a.clone(), wall, later).unwrap();
    recreate.observe_policy(&goat_policy(), later);
    drop(recreate);
    probe.confirm_success();
    drop(probe);
    assert!(
        runtime
            .list_restrictions(later)
            .iter()
            .any(|row| row.rule_id == crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE),
        "ABA recreate must keep the new episode"
    );
}

#[test]
fn probe_success_does_not_clear_newer_observation_of_leased_source() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let mut pending = runtime.acquire(a.clone(), wall, mono).unwrap();
    let mut first = runtime.acquire(a.clone(), wall, mono).unwrap();
    first.observe_policy(&goat_policy(), mono);
    drop(first);
    let later = mono + Duration::from_secs(40);
    let mut probe = runtime.acquire(a.clone(), wall, later).unwrap();
    pending.observe_policy(&goat_policy(), later);
    drop(pending);
    probe.confirm_success();
    drop(probe);
    assert!(
        runtime
            .list_restrictions(later)
            .iter()
            .any(|row| row.rule_id == crate::gateway::policy::GOAT_CREDITS_REJECTION_RULE),
        "newer observation of the leased source must survive stale success"
    );
}

#[test]
fn policy_retry_after_survives_local_clear_and_rule_delete() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let other_model = ResourceSet::fixture(1, 1, 2, &["a"], false);
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    permit.observe_policy_retry(
        crate::gateway::policy::RestrictionScope::CredentialModel,
        RetryHint::Until(wall + chrono::Duration::seconds(600)),
        mono,
    );
    drop(permit);
    let id = runtime
        .list_restrictions(mono)
        .into_iter()
        .next()
        .expect("local restriction")
        .id;
    runtime.clear_restriction(&id);
    let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(!wait.is_local_policy(), "{wait:?}");
    assert!(wait.upstream_not_before.is_some());
    assert!(runtime.inspect_admission(&other_model, wall, mono).is_ok());
    runtime.clear_source(&goat_policy().source);
    let wait = runtime.inspect_admission(&a, wall, mono).unwrap_err();
    assert!(wait.upstream_not_before.is_some());
    let expired_wall = wall + chrono::Duration::seconds(600);
    let expired_mono = mono + Duration::from_secs(600);
    runtime
        .inspect_admission(&a, expired_wall, expired_mono)
        .expect("elapsed Retry-After must admit");
    let first = runtime
        .acquire(a.clone(), expired_wall, expired_mono)
        .expect("elapsed Retry-After admits");
    let second = runtime
        .acquire(a.clone(), expired_wall, expired_mono)
        .expect("elapsed Retry-After must not stay single-flight");
    drop((first, second));
    assert_eq!(
        runtime.tracked_slot_count(),
        0,
        "expired policy Retry-After must reclaim after local clear"
    );
}

#[test]
fn expired_policy_retry_after_probe_then_concurrent_reclaim() {
    let runtime = Arc::new(RecoveryRuntime::default());
    let (wall, mono) = clock();
    let a = resource(1);
    let mut permit = runtime.acquire(a.clone(), wall, mono).unwrap();
    permit.observe_policy(&goat_policy(), mono);
    permit.observe_policy_retry(
        crate::gateway::policy::RestrictionScope::CredentialModel,
        RetryHint::Until(wall + chrono::Duration::seconds(5)),
        mono,
    );
    drop(permit);
    let due_wall = wall + chrono::Duration::seconds(5);
    let due_mono = mono + Duration::from_secs(40);
    runtime
        .inspect_admission(&a, wall, due_mono)
        .expect_err("Retry-After still in the future");
    let mut probe = runtime
        .acquire(a.clone(), due_wall, due_mono)
        .expect("elapsed Retry-After plus local wait admits a probe");
    assert!(
        runtime.acquire(a.clone(), due_wall, due_mono).is_err(),
        "due policy probe stays single-flight"
    );
    probe.confirm_success();
    drop(probe);
    let first = runtime
        .acquire(a.clone(), due_wall, due_mono)
        .expect("healthy after probe");
    let second = runtime
        .acquire(a.clone(), due_wall, due_mono)
        .expect("ordinary requests are concurrent after Retry-After recovery");
    drop((first, second));
    assert_eq!(
        runtime.tracked_slot_count(),
        0,
        "recovered policy Retry-After must not leak slots"
    );
}
