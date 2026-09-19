use super::*;
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
